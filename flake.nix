{
  description = "disk-sweep: disk cleanup built for AI agents and the humans who approve them";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      # nixpkgs 26.11 dropped x86_64-darwin; Intel Macs can use the release binaries (install.sh).
      systems = [
        "aarch64-darwin"
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
    in
    {
      packages = forAllSystems (pkgs: {
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.disk-sweep;
        disk-sweep = pkgs.rustPlatform.buildRustPackage {
          pname = "disk-sweep";
          inherit (cargoToml.workspace.package) version;

          # Only what the build reads, including files embedded with include_str!.
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./crates
              ./rules
              ./skills
              ./.claude-plugin
            ];
          };
          cargoLock.lockFile = ./Cargo.lock;

          cargoBuildFlags = [
            "--package"
            "disk-sweep"
          ];
          cargoTestFlags = [ "--workspace" ];
          # Replays the live FSEvents log, which needs fseventsd and may be unavailable in a build sandbox.
          checkFlags = [ "--skip=fsevents::tests::journal_sees_a_new_file" ];

          nativeBuildInputs = [ pkgs.makeWrapper ];
          # `ps` is the only runtime command on Linux; on macOS `ps` and `diskutil` are part of the OS.
          postInstall = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
            wrapProgram $out/bin/disk-sweep --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.procps ]}
          '';

          meta = {
            description = "Disk cleanup built for AI agents: sign-in-aware candidates, plan/apply, CLI + MCP server";
            homepage = "https://github.com/shiroinock/disk-sweep";
            license = with pkgs.lib.licenses; [
              mit
              asl20
            ];
            mainProgram = "disk-sweep";
            platforms = pkgs.lib.platforms.darwin ++ pkgs.lib.platforms.linux;
          };
        };
      });

      apps = forAllSystems (pkgs: {
        default = {
          type = "app";
          program = pkgs.lib.getExe self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          meta.description = "Run disk-sweep";
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.default ];
          packages = with pkgs; [
            clippy
            rustfmt
            rust-analyzer
          ];
        };
      });

      checks = forAllSystems (pkgs: {
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
