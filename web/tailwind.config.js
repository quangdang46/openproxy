/** @type {import('tailwindcss').Config} */
export default {
  content: [
    "./src/**/*.{astro,html,js,jsx,md,mdx,svelte,ts,tsx,vue}",
  ],
  theme: {
    extend: {
      colors: {
        // ----------------------------------------------------------
        // Claude brand & accent tones
        // ----------------------------------------------------------
        // Coral + accents use channel-format CSS vars so Tailwind opacity
        // modifiers (`bg-brand-coral/15`, `ring-brand-coral/30`) work.
        'brand-coral': 'rgb(var(--color-brand-coral-rgb) / <alpha-value>)',
        'brand-coral-active': 'rgb(var(--color-brand-coral-active-rgb) / <alpha-value>)',
        'accent-teal': 'rgb(var(--color-accent-teal-rgb) / <alpha-value>)',
        'accent-amber': 'rgb(var(--color-accent-amber-rgb) / <alpha-value>)',
        'brand-magenta': 'var(--color-brand-magenta)',
        'brand-blue': 'var(--color-brand-blue)',
        'brand-blue-deep': 'var(--color-brand-blue-deep)',
        'brand-blue-700': 'var(--color-brand-blue-700)',
        'brand-blue-200': 'var(--color-brand-blue-200)',
        'brand-cyan': 'var(--color-brand-cyan)',
        'brand-purple': 'var(--color-brand-purple)',

        // Surface tokens
        canvas: 'var(--color-canvas)',
        'surface-base': 'var(--color-surface-base)',
        'surface-soft': 'var(--color-surface-soft)',
        'surface-card': 'var(--color-surface-card)',
        'surface-cream-strong': 'var(--color-surface-cream-strong)',
        'surface-dark': 'var(--color-surface-dark)',
        'surface-dark-elevated': 'var(--color-surface-dark-elevated)',
        'surface-dark-soft': 'var(--color-surface-dark-soft)',
        hairline: 'var(--color-hairline)',
        'hairline-soft': 'var(--color-hairline-soft)',
        'footer-bg': 'var(--color-footer-bg)',

        // Text tokens
        ink: 'var(--color-ink)',
        'ink-strong': 'var(--color-ink-strong)',
        charcoal: 'var(--color-charcoal)',
        'body-strong': 'var(--color-body-strong)',
        body: 'var(--color-body)',
        slate: 'var(--color-slate)',
        steel: 'var(--color-steel)',
        stone: 'var(--color-stone)',
        muted: 'var(--color-muted)',
        'muted-soft': 'var(--color-muted-soft)',
        'on-primary': 'var(--color-on-primary)',
        'on-dark': 'var(--color-on-dark)',
        'on-dark-soft': 'var(--color-on-dark-soft)',

        // Status
        'success-bg': 'var(--color-success-bg)',
        'success-text': 'var(--color-success-text)',

        // ----------------------------------------------------------
        // Legacy aliases (do not remove — referenced across the app)
        // ----------------------------------------------------------
        brand: {
          50: 'var(--color-brand-50)',
          100: 'var(--color-brand-100)',
          200: 'var(--color-brand-200)',
          300: 'var(--color-brand-300)',
          400: 'var(--color-brand-400)',
          500: 'var(--color-brand-500)',
          600: 'var(--color-brand-600)',
          700: 'var(--color-brand-700)',
          800: 'var(--color-brand-800)',
          900: 'var(--color-brand-900)',
        },
        // `primary` must use the channel-format CSS vars so Tailwind opacity
        // modifiers (`bg-primary/10`, `border-primary/30`) actually compile —
        // pointing it at the hex `var(--color-primary)` silently dropped the
        // `/N` modifier in Tailwind v3, falling back to a gray-200 border.
        primary: {
          DEFAULT: 'rgb(var(--color-brand-coral-rgb) / <alpha-value>)',
          hover: 'rgb(var(--color-brand-coral-active-rgb) / <alpha-value>)',
          active: 'rgb(var(--color-brand-coral-active-rgb) / <alpha-value>)',
          disabled: 'var(--color-primary-disabled)',
        },
        bg: {
          DEFAULT: 'var(--color-bg)',
          alt: 'var(--color-bg-alt)',
        },
        surface: {
          DEFAULT: 'var(--color-surface)',
          2: 'var(--color-surface-2)',
          3: 'var(--color-surface-3)',
        },
        sidebar: 'var(--color-sidebar)',
        border: {
          DEFAULT: 'var(--color-border)',
          subtle: 'var(--color-border-subtle)',
        },
        text: {
          DEFAULT: 'var(--color-text)',
          main: 'var(--color-text-main)',
          muted: 'var(--color-text-muted)',
          subtle: 'var(--color-text-subtle)',
        },
      },
      borderRadius: {
        // MiniMax radius scale
        'mini-xs': 'var(--radius-xs)',
        'mini-sm': 'var(--radius-sm)',
        'mini-md': 'var(--radius-md)',
        'mini-lg': 'var(--radius-lg)',
        'mini-xl': 'var(--radius-xl)',
        'mini-xxl': 'var(--radius-xxl)',
        'mini-xxxl': 'var(--radius-xxxl)',
        hero: 'var(--radius-hero)',
        // Legacy aliases
        brand: 'var(--radius-brand)',
        'brand-lg': 'var(--radius-brand-lg)',
      },
      spacing: {
        'mini-xxs': 'var(--space-xxs)',
        'mini-xs': 'var(--space-xs)',
        'mini-sm': 'var(--space-sm)',
        'mini-md': 'var(--space-md)',
        'mini-lg': 'var(--space-lg)',
        'mini-xl': 'var(--space-xl)',
        'mini-xxl': 'var(--space-xxl)',
        'mini-xxxl': 'var(--space-xxxl)',
        'section-sm': 'var(--space-section-sm)',
        section: 'var(--space-section)',
        'section-lg': 'var(--space-section-lg)',
        hero: 'var(--space-hero)',
      },
      boxShadow: {
        soft: 'var(--shadow-soft)',
        card: 'var(--shadow-card)',
        atmospheric: 'var(--shadow-atmospheric)',
        modal: 'var(--shadow-modal)',
        warm: 'var(--shadow-warm)',
        elevated: 'var(--shadow-elevated)',
        elev: 'var(--shadow-elev)',
        focus: 'var(--shadow-focus)',
      },
      fontFamily: {
        sans: ['var(--font-sans)'],
        serif: ['var(--font-serif)'],
        mono: ['var(--font-mono)'],
        display: ['var(--font-serif)'],
      },
    },
  },
  plugins: [],
}
