# disk-sweep

AI エージェントと、それを承認する人のためのディスク整理ツールです。

[English](README.md)

よくあるクリーナーは、人が UI をクリックして使う前提で作られています。`disk-sweep` は、AI エージェント(Claude Code、Codex、MCP クライアントなど)が人と一緒に作業する前提で作っています。

- **すべて JSON で出力**:バイト数と、人が読みやすい表記の両方を返します。
- **ログインへの影響が分かる**:候補ごとに `login_risk` が付きます。ブラウザのプロファイル、Playwright や DevTools MCP のプロファイル、キーチェーンは、`Caches` の下にあっても保護します。
- **起動中のアプリを考慮する**:起動中のアプリのキャッシュは削除せず、`blocked_running` として報告します。macOS では NSWorkspace のバンドル ID で判定します。
- **見えない部分を正直に報告する**:`overview` は、ボリュームの使用量と実際に計測できた量を比べます。読めなかった場所は、理由(Unix のパーミッションか、macOS のプライバシー保護(TCC)か)と、人が自分で実行するためのコマンドと一緒に一覧にします。エージェントにはパスワードを求めないよう指示しています。
- **計画してから適用する**:エージェントは計画ファイルを作るだけです。適用するときは、各項目を最新のルールで評価し直し、状態が変わったものは飛ばします。MCP サーバーは、`--allow-apply` を付けて起動しない限り削除できません。
- **2回目以降のスキャンが速い**:ディレクトリごとの合計をインデックスに保存します。macOS では FSEvents の履歴で変更のあったディレクトリだけをスキャンし直すので、ディスク全体のスキャンが約1分から数秒になります。ほかの OS では、更新時刻が変わっていないディレクトリのファイル確認を省きます。
- **複数ユーザーに対応**:`sudo` で `--all-users` を付けると、すべてのアカウントのホームを対象にします。

## インストール

### エージェントに任せる

Claude Code や Codex など、シェルを使えるエージェントに次の文を貼り付けてください。

> `curl -fsSL https://raw.githubusercontent.com/shiroinock/disk-sweep/main/install.sh | sh -s -- --setup` で disk-sweep をインストールして、`disk-sweep doctor --human` の結果を教えて。

インストーラーは対話なし・sudo なしで動き、チェックサムを確認します。合うビルド済みバイナリがなければ `cargo install` に切り替え、最後に結果を1行の JSON で出力します。`--setup` を付けると、続けて `disk-sweep setup --yes` を実行します。

### エージェントへの組み込み

```sh
disk-sweep setup          # 試し実行。見つかったエージェントごとに、何を変えるかを表示
disk-sweep setup --yes    # 適用
disk-sweep doctor         # 確認。setup に --uninstall を付けると元に戻せます
```

Claude Code、Codex、Claude Desktop、Cursor に対応しています。設定ファイルは変更前に `*.disk-sweep-backup` としてバックアップします。

Claude Code のプラグインとしても入れられます(バイナリは別途インストールが必要です)。

```
/plugin install disk-sweep --marketplace shiroinock/disk-sweep
```

### Nix

```sh
nix run github:shiroinock/disk-sweep -- overview --human   # インストールせずに試す
nix profile install github:shiroinock/disk-sweep           # インストール
nix develop                                                 # cargo・clippy・rustfmt・rust-analyzer 入りの開発環境
```

対応システムは `aarch64-darwin`、`x86_64-linux`、`aarch64-linux` です。nixpkgs が `x86_64-darwin` のサポートを終了したため、Intel Mac ではリリースのバイナリを使ってください。Nix で入れた場合、`disk-sweep setup` は `/nix/store` のパスではなく、PATH 上のプロファイルのパスを設定に書き込みます。スキャンのインデックスの置き場所は `XDG_CACHE_HOME` で変えられます。

## 使い方

```sh
disk-sweep overview --depth 1 --human     # 容量の内訳と、計測できなかった場所
disk-sweep scan ~/Library --depth 2       # サイズのツリーと大きいファイル
disk-sweep candidates --human             # 既知のキャッシュ・ビルド成果物・VM イメージ・保護対象
disk-sweep projects ~/code --min-idle-days 30   # 放置されたプロジェクトの node_modules など
disk-sweep plan --rule npm-cache --rule xcode-derived-data
disk-sweep apply --plan /tmp/disk-sweep-123.plan.json        # 試し実行(削除しない)
disk-sweep apply --plan /tmp/disk-sweep-123.plan.json --yes  # 削除する
```

MCP サーバーとスキルの登録は `disk-sweep setup --yes` で行えます。`disk-sweep skill` でスキルの中身を出力することもできます。

ルールの書き方と構成は [README.md](README.md) を参照してください。

## 開発への参加

このリポジトリのブランチから出た PR は、Claude Code がレビューします。直す指摘が無く CI も通れば、自動でマージされます。フォークからの PR は、人がレビューしてマージします。

## ライセンス

MIT OR Apache-2.0
