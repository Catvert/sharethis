import { defineConfig } from 'vite'
import path from 'path'
import UnoCSS from 'unocss/vite'
import { presetUno, presetTypography } from 'unocss'

export default defineConfig({
  root: path.resolve(__dirname, 'src'),
  build: {
    outDir: path.resolve(__dirname, 'dist'),
    rollupOptions: {
      input: {
        main: path.resolve(__dirname, 'src/js/main.js')
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
      presets: [
        presetUno(),
        presetTypography(),
      ],
    }),
  ],
  optimizeDeps: {
    include: [
      '@tiptap/core',
      '@tiptap/starter-kit'
    ]
  },
})
