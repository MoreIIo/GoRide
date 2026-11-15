import { defineConfig } from "vite";

// Sur téléphone (tauri android dev), le serveur doit être joignable depuis l'appareil :
// Tauri fournit alors son adresse dans TAURI_DEV_HOST.
const host = process.env.TAURI_DEV_HOST;

// Réglages recommandés par Tauri : port fixe, pas d'effacement de la console.
export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    watch: { ignored: ["**/src-tauri/**", "**/target/**"] },
  },
  build: {
    target: "es2022",
    chunkSizeWarningLimit: 1500, // MapLibre pèse ~1 Mo, c'est normal
  },
});
