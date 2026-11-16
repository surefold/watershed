// The grid's main pages, in Solid and React: they load with no errors, and a
// person's edit can be undone.

import { cell, typeInto } from "./grid";
import { expect, test } from "./mocks";

const PAGES = [
  { name: "Solid", path: "/grid/" },
  { name: "React", path: "/grid/react/" },
];

// Without motion the pages do not stream prices from the start, so a cell keeps what was typed.
test.use({ reducedMotion: "reduce" });

for (const { name, path } of PAGES) {
  test(`${name} grid page: loads, and an edit is undone with Ctrl+Z`, async ({ page }) => {
    await page.goto(path);
    const grid = page.locator(".wg-body").first().locator("xpath=..");
    const first = grid.locator(".wg-rows .wg-row:not(.wg-group)").first();
    await expect(first).toBeVisible();
    const key = (await first.getByRole("rowheader").textContent())!.trim();

    const qty = await cell(grid, key, "qty");
    const was = (await qty.textContent())!.trim();
    const typed = was === "7" ? "8" : "7";
    await typeInto(grid, key, "qty", typed);
    await expect(qty).toHaveText(typed);

    await page.keyboard.press("ControlOrMeta+z");
    await expect(qty).toHaveText(was);
    // Let anything late go wrong before the errors are checked.
    await page.waitForTimeout(300);
  });
}
