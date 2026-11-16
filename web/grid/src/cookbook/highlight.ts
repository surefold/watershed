// Syntax highlighting for the cookbook's code, shared by the Solid and React
// pages. Prism turns code into HTML with a span for each token; the colours
// are in cookbook.css, for light and dark.

import Prism from "prismjs";
// Each grammar extends the ones before it, so the order matters.
import "prismjs/components/prism-typescript";
import "prismjs/components/prism-jsx";
import "prismjs/components/prism-tsx";
import "prismjs/components/prism-bash";
import "prismjs/components/prism-ini";
import "prismjs/components/prism-toml";

export type Language = "tsx" | "typescript" | "bash" | "ini" | "toml";

/** The code as HTML, its tokens in spans. Prism escapes the code itself. */
export function highlight(code: string, language: Language): string {
  return Prism.highlight(code, Prism.languages[language], language);
}
