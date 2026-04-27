// ESLint flat config — keep this minimal. The SDK is small enough
// that the surface to lint is bounded; we lean on `tsc --strict` for
// the heavy lifting and use ESLint for stylistic + correctness rules
// the type checker can't catch.
import tseslint from "@typescript-eslint/eslint-plugin";
import tsparser from "@typescript-eslint/parser";

export default [
  {
    files: ["src/**/*.ts"],
    languageOptions: {
      parser: tsparser,
      parserOptions: {
        ecmaVersion: 2022,
        sourceType: "module",
      },
    },
    plugins: {
      "@typescript-eslint": tseslint,
    },
    rules: {
      "no-console": ["error", { allow: ["warn", "error"] }],
      "no-unused-vars": "off",
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_" },
      ],
      "@typescript-eslint/no-explicit-any": "off",
      "no-implicit-coercion": "error",
      eqeqeq: ["error", "always", { null: "ignore" }],
    },
  },
];
