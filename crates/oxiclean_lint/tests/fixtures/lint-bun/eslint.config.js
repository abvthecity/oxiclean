import oxlint from 'eslint-plugin-oxlint';
import tseslint from 'typescript-eslint';

export default [
  { ignores: ['packages/app/dist/**'] },
  ...tseslint.configs.recommended,
  {
    files: ['**/*.ts'],
    languageOptions: {
      parserOptions: { project: true, tsconfigRootDir: import.meta.dirname },
    },
  },
  ...oxlint.buildFromOxlintConfigFile('./.oxlintrc.json'),
];
