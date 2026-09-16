# 貢獻指南

## 變更範圍與文件維護

開發與文件修改以[系統架構](docs/architecture.md)及[安全政策](SECURITY.md)為依據。涉及行為、介面或資料處理的變更，須同步更新相關規格與[開發狀態](docs/README.md)。

文件採客觀技術文體，區分設計要求、已實作能力及驗證證據。正文使用繁體中文，檔名、程式碼與技術標識使用英文，文字檔統一 LF；標準授權原文保持完整。

## 驗證與 review

1. 檢查工作目錄及暫存區，保留既有 staged／unstaged 改動。
2. 執行與變更相稱的驗證。文件檢查連結、術語、狀態一致性及敏感資料；程式碼修改驗證受影響行為及必要邊界。
3. 以明確檔案清單或區塊選取暫存變更，排除無關內容。
4. 檢查 `git diff --cached` 並執行 `git diff --cached --check`，提供變更摘要、驗證結果及建議提交訊息。
5. **Agent 完成 stage 後交付 review，commit 由使用者手動執行。** 不自動 commit、amend、推送或發布。

review 後的修訂須重新驗證並更新 stage。既有暫存內容保留原狀，交付時明確列出變更範圍及尚未驗證事項。

## Commit message

採用 Conventional Commits 與 gitmoji：

```text
<type>(<scope>): <gitmoji> <description>

[optional body]

[optional footer(s)]
```

| 欄位 | 格式 |
| --- | --- |
| type | 英文小寫，例如 `feat`、`fix`、`docs`、`refactor`、`test`、`build`、`ci`、`chore` |
| scope | 英文，可省略 |
| gitmoji | 位於冒號及空格後，例如 `✨`、`🐛`、`📝`、`♻️`、`✅`、`🎉` |
| description | 簡短繁體中文敘述，技術標識保留英文 |
| body、footer | 選填，使用英文 |

不相容變更使用 `!` 或英文 `BREAKING CHANGE:` footer。

```text
chore(repo): 🎉 初始化倉庫與專案規範
docs(architecture): 📝 明確定義控制與流量平面的職責
```

格式依據：[Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/) · [gitmoji](https://gitmoji.dev/)。

## 授權與資產

貢獻須符合 `AGPL-3.0-only` 授權。第三方內容須確認使用與散布權利並保留必要聲明。遊戲素材、私有部署資料、秘密及未確認權利的資產不納入版本庫。
