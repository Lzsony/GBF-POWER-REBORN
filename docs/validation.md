# 驗證紀錄

## 驗證範圍

0.1.0 的測試以隔離環境驗證直連／上游代理、基本快取、PAC、設定、介面及原生常駐。自建加速、Control 與 Gateway 不在本版本驗證範圍。

## 執行入口

```sh
npm ci
npm run build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
npm run test:pac
npx playwright install chromium webkit
npm run test:ui
npm run test:ui -- --browser=webkit
```

macOS 原生驗收使用獨立資料及識別：

```sh
cargo build -p gbf-power-reborn --features internal-test,tauri/custom-protocol --locked
node scripts/test-macos-native.mjs target/debug/gbf-power-reborn
cargo run -p gbf-power-reborn --example native_lifecycle --locked
cargo test -p gbf-core native_certificate_and_credential_round_trip --locked -- --ignored
```

CA／Keychain 測試可能要求系統確認。只有完成安裝、使用、移除與清理的操作才能記錄為通過。介面驗證不建立截圖、錄影或 trace。

## Windows CI

[Windows 工作流程](../.github/workflows/check.yml) 在 `windows-2025` 執行鎖定依賴安裝、前端建置、Rust 格式／靜態檢查、核心與桌面單元測試、PAC 檢查、Chromium 互動及布局測試、原生 EXE 建置，以及 ZIP／SHA-256 完整性驗證。

CI 產物依賴系統 WebView2 Runtime，未做 Authenticode 簽署。自動測試成功不等於 Windows 實際 WebView2 互動、憑證信任、托盤或完整遊戲流程已驗收。工作流程須在推送至 GitHub 或手動觸發後才能取得實際執行結果。

## 本機套件

`npm run package:macos` 只在 Apple Silicon 上建置，輸出 App、DMG、SHA-256 與逐檔 manifest 至 `artifacts/macos/`。檢查識別、版本、arm64 架構、macOS 13.0 最低版本宣告、授權文件、ad-hoc 簽章、測試入口排除及 DMG 唯讀掛載內容一致性。

套件未經 Developer ID 簽署或 Apple 公證；manifest 的 `sourceFiles` 描述建置內容，`baseCommit` 僅為當時 HEAD，不代表尚未提交的原始碼已納入該 commit。

## 本機結果（2026-09-16）

環境為 macOS 27、Apple Silicon；以下檢查均在本專案執行。

| 檢查 | 結果 |
| --- | --- |
| 前端 TypeScript／Vite 建置 | 通過 |
| Rust 格式與 Clippy（workspace、all-targets） | 通過 |
| 核心單元／隔離網路測試 | 86 通過、0 失敗、2 項預設忽略 |
| 桌面單元測試 | 7 通過 |
| PAC 規則 | 81 組案例 × 2 個埠號通過 |
| Chromium 介面測試 | 42 通過 |
| WebKit 介面測試 | 42 通過 |
| 真實 WKWebView | 啟動錯誤、全新設定及重啟共 3 組情境通過 |
| 原生生命週期 | 6 項檢查通過，含托盤、隱藏、連線保留及停止釋放埠號 |
| 隔離 CA／Keychain | 另行 opt-in 執行 1 項通過，完成測試材料清理 |
| macOS arm64 release build | 通過 |
| App／DMG 封裝 | arm64、版本／識別、授權文件、ad-hoc 簽章及唯讀掛載內容一致性通過 |
| Windows CI | 工作流程已建立，尚未在 GitHub 執行 |

核心預設忽略項目分別為受控子程序替身與 opt-in CA／Keychain 測試；前者由命令測試啟動，後者已另行驗證。介面測試使用模擬 IPC，WKWebView 與原生生命週期另行驗證實際整合。

詳細輸出保存在本機 `artifacts/validation/`；套件完整性以封裝入口成功結果及 `artifacts/macos/manifest.json` 為準。未驗證 Windows 真機、最低 macOS 版本及完整遊戲操作；macOS Intel 不在本版本範圍。
