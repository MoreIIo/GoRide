import { defineConfig } from "vite";

// Réglages recommandés par Tauri : port fixe, pas d'effacement de la console.
export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**", "**/target/**"] },
  },
  build: {
    target: "es2022",
    chunkSizeWarningLimit: 1500, // MapLibre pèse ~1 Mo, c'est normal
  },
});
