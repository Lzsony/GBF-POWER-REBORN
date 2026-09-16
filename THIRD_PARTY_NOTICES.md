# 第三方聲明

## 字型與圖示

介面使用 Noto Sans TC，著作權屬 The Noto Project Authors，依 SIL Open Font License 1.1 提供；完整條款見 [OFL-NotoSansTC.txt](docs/OFL-NotoSansTC.txt)。

GPR 字母圖示由本專案以 SVG 幾何路徑建立，隨專案依 AGPL-3.0-only 提供。

## 軟體依賴

Rust 與 npm 依賴保留各自授權。`Cargo.lock` 與 `package-lock.json` 固定解析版本；`scripts/collect-licenses.py` 收集已解析依賴的授權文字與版本清單，隨套件置於 `licenses/dependencies`。

發布包未附授權文字的依賴，由 `docs/dependency-licenses/manifest.json` 記錄其上游聲明、作者及版本來源，並搭配 SPDX 標準授權全文。來源類型與 SHA-256 分別標示，metadata 或聲明不視為完整授權文字。

Windows 執行環境使用 Microsoft Edge WebView2。CI 測試套件依賴系統已安裝的 WebView2 Runtime，不包含其再分發副本。

專案致謝見 [README](README.md)。本文件不授予第三方商標或遊戲素材的權利。
