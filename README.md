# GBF POWER REBORN

GBF POWER REBORN 是面向 macOS 與 Windows 的網路體驗優化專案，透過本地靜態資源快取與動態流量透明轉發，減少靜態資源的重複下載並降低載入延遲。本專案僅供學習、研究與技術交流。

## 開發狀態

**0.3.0 客戶端開發版。** 提供直連、上游代理與本機快取，介面支援简体、繁體、日本語與 English。加速選項尚未開放；Control、SSH Gateway 與服務端部署工具尚未實作。平台驗證範圍見[驗證紀錄](docs/validation.md)。

## 平台與技術棧

| 層級 | 平台與技術 |
| --- | --- |
| 桌面客戶端 | macOS arm64、Windows x64；Tauri 2、React、Rust |
| 服務端規劃 | Go、SQLite、Bubble Tea TUI、獨立 SSH Gateway |

服務端設計區分控制平面與流量平面：Control 負責帳號、裝置、節點及授權管理；Gateway 負責遊戲流量轉發。兩者即使部署於同一主機，仍使用獨立服務與資料。

## 接入方式

瀏覽器代理插件載入 PAC，將符合分流規則的請求交由本機代理處理。客戶端的上游模式決定該流量的出口路徑。

| 模式 | 出口路徑 |
| --- | --- |
| 直連 | 本機代理直接連接原始服務 |
| 上游代理 | 經指定的 HTTP 或 SOCKS5 代理連接原始服務 |
| 自部署節點加速 | 尚未開放，選項停用 |

## 功能範圍

現階段的應用支援範圍暫限於 Granblue Fantasy（GBF）。

- **靜態資源快取**：在本機保存符合白名單及快取條件的素材，包含素材預取、記憶體預熱與可取消的快取體檢，維持內容完整性與上游快取語意。詳見[快取規格](docs/cache.md)。靜態 HTTPS 快取的本機 CA 信任與 TLS 終止範圍見[架構規格](docs/architecture.md)。
- **動態流量轉發**：維持瀏覽器與原始服務之間的端到端 TLS；轉發層不解密、不修改、不快取、不重播動態流量。
- **統計與常駐**：顯示請求、快取下載、命中率、即時流量及 JP／Steam 公開頁品質；關閉或最小化視窗後保持背景執行。

自建部署為後續功能，範圍限於指引與腳本模板，不提供公共節點。

遊戲自動化、腳本注入、遊戲資料修改及代替使用者操作遊戲 API 均不屬於功能範圍。

## 使用與開發

1. 啟動客戶端，選擇直連或填入上游代理 URL。URL 測試確認 TCP 可達後，按「保存」。
2. 啟動本機代理，在瀏覽器代理插件的 PAC 情境中設定 `http://127.0.0.1:8123/proxy.pac`。
3. HTTPS 靜態快取須先停止代理，在憑證選單安裝並信任本機 CA，再開啟本地快取。
4. 退出程式前將插件切回直連；程式不修改系統代理或插件設定。

```sh
npm ci
npm run desktop
```

開發環境需要 Node.js 22+、Rust 與平台桌面工具鏈；macOS 使用 Xcode，Windows 使用 MSVC Build Tools 與 Windows SDK。macOS 套件入口為 `npm run package:macos`；Windows 自動檢查由 GitHub Actions 執行，結果及限制見[驗證紀錄](docs/validation.md)。

設定、快取與執行資料位於 macOS 的 `~/Library/Application Support/GBF Power Reborn`，或 Windows 的 `%LOCALAPPDATA%/GBF Power Reborn`。上游密碼與 CA 私鑰使用作業系統秘密儲存。詳細操作見[客戶端操作](docs/client.md)。

## 文件

- [文件索引與開發狀態](docs/README.md)
- [系統架構](docs/architecture.md)
- [本機快取與背景工作](docs/cache.md)
- [資料處理與隱私](docs/privacy.md)
- [自建服務部署規格](docs/self-hosted-gateway.md)
- [安全政策](SECURITY.md)
- [貢獻指南](CONTRIBUTING.md)

## 致謝

本專案受 [GBF-A](https://github.com/Sagisawa/GBF-Accelerator) 與 [GBF-P](https://github.com/404-500-505/GBF-POWER) 啟發，謹向兩個專案的作者、維護者與貢獻者致意，感謝其對社群的投入與技術分享。

## 授權與專案聲明

本專案採用 **GNU Affero General Public License v3.0 only**（`AGPL-3.0-only`），完整條款見 [LICENSE](LICENSE)。用途說明不構成額外授權限制。

本專案為獨立非官方專案，與任何遊戲開發商、發行商或平台均無隸屬關係，亦未獲其授權或背書。相關名稱、商標及遊戲素材的權利歸各權利人所有，詳見[免責與權利聲明](DISCLAIMER.md)。
