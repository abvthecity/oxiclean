import { defineConfig } from 'eslint/config';
import importPlugin from 'eslint-plugin-import';
import tseslint from 'typescript-eslint';

const typed = process.env.TYPED === '1';

export default defineConfig([
  tseslint.configs.base,
  {
    languageOptions: {
      parserOptions: { projectService: true, tsconfigRootDir: import.meta.dirname },
    },
  },
  {
    files: ['scripts/**'],
    languageOptions: { parserOptions: { projectService: typed } },
  },
  {
    plugins: { import: importPlugin },
    rules: { 'import/no-deprecated': 'warn' },
  },
] satisfies Parameters<typeof defineConfig>[0]);
