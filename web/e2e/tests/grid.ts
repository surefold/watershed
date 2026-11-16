// Finding things in a drawn grid by what a person sees: a row by its key, a
// cell by its column's header.

import { expect, type Locator } from "@playwright/test";

/** The row whose key cell reads exactly `key`. */
export function row(grid: Locator, key: string): Locator {
  return grid.locator(".wg-rows .wg-row").filter({ has: grid.page().getByRole("rowheader", { name: key, exact: true }) });
}

/** The position of the column headed `column` (0 is the key column). */
export async function columnIndex(grid: Locator, column: string): Promise<number> {
  await expect(grid.locator(".wg-hname").first()).toBeVisible();
  const names = await grid.locator(".wg-hrow .wg-hname").allTextContents();
  const at = names.findIndex((name) => name.trim() === column);
  if (at < 0) throw new Error(`No column "${column}" in ${JSON.stringify(names)}`);
  return at;
}

/** The cell in the row keyed `key` under the column headed `column`. */
export async function cell(grid: Locator, key: string, column: string): Promise<Locator> {
  const at = await columnIndex(grid, column);
  return row(grid, key).locator(".wg-cell").nth(at);
}

/** Types `text` into a cell as a person does: double-click, select all, type, Enter. */
export async function typeInto(grid: Locator, key: string, column: string, text: string) {
  const target = await cell(grid, key, column);
  await target.dblclick();
  const editor = grid.locator("input.wg-editor");
  await expect(editor).toBeFocused();
  await editor.press("ControlOrMeta+a");
  await editor.pressSequentially(text);
  await editor.press("Enter");
  await expect(editor).toHaveCount(0);
}
