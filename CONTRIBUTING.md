# 貢獻指南

## 變更範圍與文件維護

開發與文件修改以[系統架構](docs/architecture.md)及[安全政策](SECURITY.md)為依據。涉及行為、介面或資料處理的變更，須同步更新相關規格與[開發狀態](docs/README.md)。

文件採客觀技術文體，區分設計要求、已實作能力及驗證證據。正文使用繁體中文，檔名、程式碼與技術標識使用英文，文字檔統一 LF；標準授權原文保持完整。

## 驗證與 review

1. 檢查工作目錄及暫存區，保留既有 staged／unstaged 改動。
2. 執行與變更相稱的驗證。文件檢查連結、術語、狀態一致性及敏感資料；程式碼修改驗證受影響行為及必要邊界。
3. 依[版本管理](#版本管理)判定是否升版，完成必要同步後，以明確檔案清單或區塊選取暫存變更，排除無關內容。
4. 檢查 `git diff --cached` 並執行 `git diff --cached --check`，提供變更摘要、驗證結果、版本判定及完整提交訊息。
5. **Agent 在 stage 後提供可編輯的 message 確認表單。使用者修改並確認後，Agent 以最終訊息提交當次已審閱的暫存內容，不重複詢問。** 確認前不提交，也不自行 amend、推送或發布。

review 後的修訂須重新驗證並更新 stage 與 message，保留使用者的訊息修改。確認後如暫存內容變動，須重新交付 review。既有暫存內容保留原狀，交付時明確列出變更範圍及尚未驗證事項。

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
| body | 選填，以繁體中文濃縮主要變更與必要驗證結果，技術標識保留英文 |
| footer | 選填，使用英文 |

不相容變更使用 `!` 或英文 `BREAKING CHANGE:` footer。

```text
chore(repo): 🎉 初始化倉庫與專案規範
docs(architecture): 📝 明確定義控制與流量平面的職責
```

格式依據：[Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/) · [gitmoji](https://gitmoji.dev/)。

## 版本管理

版本採 `MAJOR.MINOR.PATCH`，每批功能變更在 stage 前自動判定是否升版。規則參考 [SemVer](https://semver.org/spec/v2.0.0.html) 與 [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/)；`0.x` 的不相容變更處理屬本專案的開發期慣例。

### 升版判定

| 變更 | 規則 | 範例 |
| --- | --- | --- |
| 新增使用者功能、實質擴充能力 | MINOR +1，PATCH 歸零 | `0.2.0 → 0.3.0` |
| 修復錯誤、安全問題或相容的效能改善 | PATCH +1 | `0.2.0 → 0.2.1` |
| `0.x` 階段的不相容變更 | MINOR +1，PATCH 歸零，並標記 `!` 或 `BREAKING CHANGE` | `0.2.0 → 0.3.0` |
| `1.0.0` 之後的不相容變更 | MAJOR +1，其餘歸零 | `1.2.3 → 2.0.0` |
| 純文件、測試、格式、無行為改變的重構或 CI 維護 | 不升版 | 維持原版本 |

判定以實際影響為準，不只看 commit type。建置或依賴變更若影響交付包的安裝、啟動或運行，按修復或功能變更處理。

同批包含多種改動時取最高級別，以該批開始時的已提交版本為基準，只升一次；review 修訂不重複累加。升至 `1.0.0` 須由使用者明確決定。使用者指定版本時優先採用，不自行降版或重用已發布版本承載不同內容。

### 同步範圍

每次升版同步檢查：

- npm 主套件、lockfile 根套件、兩個 Rust workspace package 及其 lockfile 條目、Tauri 版本。
- 介面版本顯示與測試期望值、macOS／Windows 封裝檔名、manifest、App／PE 版本檢查。
- 描述目前版本的文件；歷史驗證紀錄與第三方依賴版本保持原值。

應用版本與 `schemaVersion` 分開管理。只有設定格式相容契約需要變更時才調整 schema；新增可選欄位不自動提高 schema。

### 驗證與交付

驗證以版本一致性、介面版本顯示及實際交付套件資訊為主。只有版本文字改動時，不重跑無關功能測試；已發布套件不得以同版本覆蓋不同內容。

交付摘要列出「原版本 → 新版本」及判定理由，或說明本次不升版。版本更新與相應功能／修復一併暫存，由使用者審閱並確認提交訊息；不自動建立 tag、Release、commit 或推送。

## 授權與資產

貢獻須符合 `AGPL-3.0-only` 授權。第三方內容須確認使用與散布權利並保留必要聲明。遊戲素材、私有部署資料、秘密及未確認權利的資產不納入版本庫。
