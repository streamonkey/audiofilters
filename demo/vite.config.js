import { defineConfig } from 'vite';

export default defineConfig({
  server: {
    // The package is symlinked from ../npm.
    fs: { allow: ['..'] },
  },
});
