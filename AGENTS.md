# Agent Instructions

## 文件與溝通

- 使用繁體中文敘述；檔名、程式碼與技術標識使用英文，標準授權全文保留英文。
- 文件採客觀技術文體，依職責、介面、資料流、約束及驗收條件組織內容。
- 以 [README](README.md) 為入口，按任務需要閱讀[文件索引](docs/README.md)及相關規格。
- 架構決策以[系統架構](docs/architecture.md)為準；修改技術契約時同步更新相關文件。
- 明確區分設計要求、實作狀態與驗證結果；未定義的細節標示待定，不補寫成既定契約。

## 版本控制

- 修改前檢查 staged／unstaged 狀態，保留既有改動，不重設、覆蓋或擅自取消暫存。
- 完成修改與驗證後，以明確檔案清單或區塊選取暫存任務範圍；同檔案改動無法安全區分時，先說明衝突。
- 交付 staged diff、變更摘要、驗證結果與建議 commit message，由使用者 review 後手動 commit。
- Agent 不執行 commit、amend 或建立隱含 commit 的操作，不自行推送、設定 remote 或發布。
- review 後的修訂須重新驗證、更新 stage，並說明暫存內容的變更。
- 提交格式為 `<type>(<scope>): <gitmoji> <description>`，scope 可省略；description 使用繁體中文，type、scope、body、footer 使用英文。gitmoji 放在冒號後，詳見[貢獻指南](CONTRIBUTING.md)。

## 驗證與交付

- 驗證力度與改動相稱；文件檢查連結、術語、狀態一致性及 `git diff --cached --check`，不新增複述文件內容的功能測試。
- 分別報告已驗證、未驗證與受阻事項。介面驗證使用互動、元素及布局斷言，不產生截圖、錄影或視覺 trace。
- 暫存內容排除真實節點、本機設定、秘密、憑證私鑰、遊戲素材、快取、依賴目錄與建置產物；保留公開範例與依賴鎖定檔。
