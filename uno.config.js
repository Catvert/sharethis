import { defineConfig } from "unocss";
import { presetUno, presetTypography } from "unocss";
import presetWebFonts from '@unocss/preset-web-fonts'

export default defineConfig({
  presets: [
    presetUno(), 
    presetTypography({
      cssExtend: {
        'p, li': {
          'line-height': '1.2',
          'margin': '0.5em 0',
        },
      },
    }),
    presetWebFonts({
      provider: 'bunny',
      fonts: {
        'sans' : ['Roboto'],
        'inter': 'Inter',
        'mono': ['Fira Code'],
      },
    }),
  ],
  theme: {
    colors: {
      primary: "#61AFEF",
    },
  },
  shortcuts: {
    "bg-base": "bg-white dark:bg-[#2E2F37]",
    "text-base": "text-gray-900 dark:text-gray-100",
    "border-base": "border-gray-200 dark:border-gray-700",
  },
});
