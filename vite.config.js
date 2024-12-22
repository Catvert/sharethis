import { defineConfig } from 'vite'
import path from 'path'
import UnoCSS from 'unocss/vite'

export default defineConfig({
  root: path.resolve(__dirname, 'src'),
  build: {
    outDir: path.resolve(__dirname, 'dist'),
    manifest: true,
    rollupOptions: {
      input: {
        main: path.resolve(__dirname, 'src/js/index.js'),
        room: path.resolve(__dirname, 'src/js/room.js')
      }
    }
  },
  plugins: [
    UnoCSS({
      content: {
        filesystem: [
          '../templates/**/*.html',  // since root is in 'src', we need to go up one level
          './**/*.{js,ts,jsx,tsx}',  // for files in src
        ],
        // Enable scanning for all changed files
        pipeline: {
          include: [/\.(vue|svelte|[jt]sx|mdx?|astro|elm|php|phtml|html|rs)?$/],
        }
      },
      // Enable logging to debug what files are being scanned
      logger: {
        debug: true 
      },
    }),
  ],
})
