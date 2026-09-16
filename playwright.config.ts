import { defineConfig } from '@playwright/test';
export default defineConfig({
  testDir: './tests/ui', fullyParallel: true,
  use: { baseURL: 'http://127.0.0.1:1420', locale: 'zh-TW', viewport: { width: 400, height: 520 }, screenshot: 'off', trace: 'off', video: 'off' },
  webServer: { command: 'npm run dev', url: 'http://127.0.0.1:1420', reuseExistingServer: !process.env.CI },
});
