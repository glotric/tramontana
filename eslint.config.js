import js from "@eslint/js";
import prettier from "eslint-config-prettier";
import reactHooks from "eslint-plugin-react-hooks";
import globals from "globals";
import tseslint from "typescript-eslint";

export default tseslint.config(
  {
    ignores: [
      "**/dist/**",
      "**/target/**",
      "**/node_modules/**",
      "apps/desktop/src-tauri/gen/**",
      // Supplied design-reference prototypes are inspected, not shipped or
      // maintained as first-party application code.
      "design/UI/**",
      // Generated from the Rust contracts; regenerate rather than lint-fix.
      "packages/shared/src/generated/**",
    ],
  },
  js.configs.recommended,
  tseslint.configs.strictTypeChecked,
  tseslint.configs.stylisticTypeChecked,
  {
    languageOptions: {
      parserOptions: {
        projectService: true,
        tsconfigRootDir: import.meta.dirname,
      },
    },
    rules: {
      // Network access belongs behind a core-owned adapter that a routing
      // decision and a permission check can see.
      "no-restricted-globals": [
        "error",
        { name: "fetch", message: "Network access belongs behind a core-owned adapter." },
      ],
      // Version numbers and counts are legitimately interpolated into strings;
      // the rule's purpose is catching objects and nullables.
      "@typescript-eslint/restrict-template-expressions": ["error", { allowNumber: true }],
      "@typescript-eslint/consistent-type-imports": [
        "error",
        { fixStyle: "separate-type-imports" },
      ],
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_" },
      ],
    },
  },
  {
    files: ["apps/desktop/src/**/*.{ts,tsx}"],
    extends: [reactHooks.configs.flat["recommended-latest"]],
    languageOptions: { globals: globals.browser },
  },
  {
    // Node types are available in this package for the fixture test, so make
    // the boundary explicit: shipped shared code stays runtime-neutral.
    files: ["packages/shared/src/**/*.ts"],
    ignores: ["packages/shared/src/**/*.test.ts"],
    rules: {
      "no-restricted-imports": [
        "error",
        {
          patterns: [
            {
              group: ["node:*"],
              message: "Shared contract code must run in the browser as well as Node.",
            },
          ],
        },
      ],
    },
  },
  {
    files: ["**/*.test.{ts,tsx}", "apps/desktop/tests/**/*.ts"],
    rules: {
      // Test doubles are intentionally loosely typed.
      "@typescript-eslint/unbound-method": "off",
      "@typescript-eslint/no-unsafe-assignment": "off",
      "@typescript-eslint/no-unsafe-argument": "off",
    },
  },
  {
    files: [
      "scripts/**/*.mjs",
      "apps/desktop/e2e/**/*.mjs",
      "*.config.{js,ts}",
      "**/vite.config.ts",
    ],
    languageOptions: { globals: globals.node },
  },
  {
    // Build config lives outside the app tsconfig so browser code cannot pick
    // up Node globals; point the type-aware rules at its own project.
    files: ["apps/desktop/vite.config.ts"],
    languageOptions: {
      parserOptions: {
        projectService: false,
        project: ["./apps/desktop/tsconfig.node.json"],
        tsconfigRootDir: import.meta.dirname,
      },
    },
  },
  {
    files: ["scripts/**/*.mjs", "apps/desktop/e2e/**/*.mjs", "eslint.config.js"],
    extends: [tseslint.configs.disableTypeChecked],
  },
  prettier,
);
