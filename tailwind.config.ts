/** @type {import('tailwindcss').Config} */
export default {
  darkMode: "class",
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      fontFamily: {
        sans: ['"Noto Sans"', "system-ui", "sans-serif"],
        serif: ['"Noto Serif"', "Georgia", "serif"],
      },
      colors: {
        cream: {
          50: "#FDFBF7",
          100: "#F9F6F0",
          200: "#F3EDE4",
          300: "#E8DFD3",
          400: "#D4C8B8",
        },
        surface: {
          50: "#FDFBF7",
          100: "#F9F6F0",
          200: "#F3EDE4",
          300: "#E8DFD3",
          700: "#2C2A27",
          800: "#1E1D1A",
          850: "#252420",
          900: "#171614",
        },
        warm: {
          50: "#FAF8F5",
          100: "#F5F2ED",
          200: "#EDE8E0",
          300: "#DDD6CB",
          400: "#B5ADA2",
          500: "#8C8580",
          600: "#6B655F",
          700: "#3D3935",
          800: "#2A2724",
          900: "#1A1815",
        },
      },
      animation: {
        "pulse-ring": "pulse-ring 1.8s cubic-bezier(0.4, 0, 0.6, 1) infinite",
      },
      keyframes: {
        "pulse-ring": {
          "0%": { transform: "scale(1)", opacity: "0.6" },
          "50%": { transform: "scale(1.8)", opacity: "0" },
          "100%": { transform: "scale(1)", opacity: "0" },
        },
      },
    },
  },
  plugins: [],
};
