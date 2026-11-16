// Reading the engine's view state, for Table and WorkerTable alike. Kept
// apart from table.ts, which loads the engine, so a page using only a
// WorkerTable does not.

import type { ViewState } from "./table.js";

/** The engine's view state (columns by position) by column name. */
export function viewStateOf(json: string, names: string[]): ViewState {
  const raw = JSON.parse(json) as { sort: [number, boolean][]; filters: Record<string, string>; search: string; group: number };
  return {
    sort: raw.sort.map(([col, desc]) => ({ column: names[col], desc })),
    filters: Object.fromEntries(Object.entries(raw.filters).map(([col, text]) => [names[Number(col)], text])),
    search: raw.search,
    group: raw.group < 0 ? null : names[raw.group],
  };
}

/** A total as the engine gives it (a number, a decimal as text, or nothing), as text. */
export function textOf(total: number | string | null | undefined): string | null {
  return total === null || total === undefined ? null : String(total);
}

/** A total as text, as the nearest number. */
export function numberOf(total: string | null): number | null {
  return total === null ? null : Number(total);
}
