import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
export default defineConfig({
  plugins: [react()],
  build: {
    rollupOptions: { output: { entryFileNames: 'assets/app.js', chunkFileNames: 'assets/[name]-[hash].js', assetFileNames: asset => asset.names.some(name => name.endsWith('.css')) ? 'assets/index.css' : 'assets/[name][extname]' } },
    cssCodeSplit: false,
  },
});
