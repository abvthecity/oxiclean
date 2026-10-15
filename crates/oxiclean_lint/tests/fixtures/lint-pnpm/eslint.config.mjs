import importX from 'eslint-plugin-import-x';
import tseslint from 'typescript-eslint';

export default tseslint.config(
  ...tseslint.configs.recommendedTypeChecked,
  {
    plugins: { 'import-x': importX },
    languageOptions: {
      parserOptions: {
        project: ['./packages/*/tsconfig.json'],
      },
    },
    rules: {
      'import-x/no-cycle': 'error',
    },
  },
  {
    files: ['**/*.test.ts'],
    rules: { 'import-x/no-cycle': 'off' },
  },
);
