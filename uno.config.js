import { defineConfig } from "unocss";
import { presetUno, presetTypography } from "unocss";

export default defineConfig({
  theme: {
    colors: {
      primary: "#3B82F6",
    },
  },
  rules: [["bg-primary", { background: "#3B82F6" }]],
});
