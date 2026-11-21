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

MCP サーバーとして使う場合は `disk-sweep mcp` を登録します。Claude のスキルとして使う場合は、`skill/disk-sweep` をスキルのフォルダにコピーします。

ルールの書き方と構成は [README.md](README.md) を参照してください。

## ライセンス

MIT OR Apache-2.0
