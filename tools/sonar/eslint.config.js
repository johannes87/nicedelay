// SonarQube's JavaScript/TypeScript rules (the "Sonar way" set from eslint-plugin-sonarjs),
// run over the whole project. Used by `npm run lint:sonar` in the repository root.
import path from 'node:path';
import sonarjs from 'eslint-plugin-sonarjs';
import tseslint from 'typescript-eslint';

const ROOT = path.resolve(import.meta.dirname, '../..');

export default [
  { ignores: ['**/node_modules/**', 'dist/**', 'tools/**'] },
  {
    files: ['**/*.ts', '**/*.tsx'],
    languageOptions: {
      parser: tseslint.parser,
      parserOptions: { projectService: true, tsconfigRootDir: ROOT },
    },
  },
  sonarjs.configs.recommended,
];
