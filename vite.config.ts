import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

const bundleId = "com.hetianxia.app";
const home = process.env.HOME ?? "";
const appDataDir =
  process.platform === "darwin"
    ? `${home}/Library/Application Support/${bundleId}`
    : process.platform === "win32"
      ? `${process.env.APPDATA ?? `${home}\\AppData\\Roaming`}\\${bundleId}`
      : `${process.env.XDG_DATA_HOME || `${home}/.local/share`}/${bundleId}`;

export default defineConfig({
  plugins: [react()],
  base: "./",
  define: {
    __HETIANXIA_APP_DATA_DIR__: JSON.stringify(appDataDir),
  },
  server: { port: 43127, host: "127.0.0.1", strictPort: true },
});
