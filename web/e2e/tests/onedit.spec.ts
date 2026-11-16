// The grid's onEdit: what a person changes is reported, cell by cell, the same
// for a Table and a WorkerTable; what code changes is not.

import { expect, test, type Page } from "@playwright/test";
import type { Edits } from "@surefold/grid";
import { cell, row, typeInto } from "./grid";
import { watchErrors } from "./mocks";

for (const kind of ["table", "worker"] as const) {
  test.describe(`onEdit with a ${kind === "table" ? "Table" : "WorkerTable"}`, () => {
    let errors: string[];

    test.beforeEach(async ({ page }) => {
      errors = watchErrors(page);
      await page.goto(`/e2e/onedit.html?table=${kind}`);
      await expect(page.locator("body")).toHaveAttribute("data-ready", kind);
      await expect(row(page.locator("#grid"), "d")).toBeVisible();
    });

    test.afterEach(async ({ page }) => {
      expect(await page.evaluate(() => window.fixture.errors), "the grid's onError").toEqual([]);
      expect(errors, "page and console errors").toEqual([]);
    });

    test("a write, awaited, is in the totals and rows read after it", async ({ page }) => {
      // With a WorkerTable the totals come from the worker: an awaited write
      // resolves once they include it, so reading straight after sees it.
      const read = await page.evaluate(async () => {
        const t = window.fixture.table;
        const seen: unknown[] = [];
        await t.set("a", "qty", 100);
        seen.push(t.total("qty"), t.size);
        await t.put("e", { name: "Elderberry", qty: 5, price: 50 });
        seen.push(t.total("qty"), t.size);
        await t.drop("b");
        seen.push(t.total("qty"), t.size);
        return seen;
      });
      // 100 + 2 + 3 + 4; then with e's 5; then without b's 2.
      expect(read).toEqual([109, 4, 114, 5, 112, 4]);
      // Writes from code are not edits.
      await expectEdits(page, []);
    });

    test("typing into a cell, then undo and redo", async ({ page }) => {
      const grid = page.locator("#grid");
      await typeInto(grid, "a", "qty", "5");
      await expect(await cell(grid, "a", "line")).toHaveText("50");
      await expectEdits(page, [{ cause: "edit", cells: [{ key: "a", column: "qty", before: 1, after: 5 }], removed: [] }]);

      // Text too.
      await typeInto(grid, "b", "name", "Blueberry");
      await expectEdits(page, [{ cause: "edit", cells: [{ key: "b", column: "name", before: "Banana", after: "Blueberry" }], removed: [] }], 1);

      await page.keyboard.press("ControlOrMeta+z");
      await expect(await cell(grid, "b", "name")).toHaveText("Banana");
      await expectEdits(page, [{ cause: "undo", cells: [{ key: "b", column: "name", before: "Blueberry", after: "Banana" }], removed: [] }], 2);

      await page.keyboard.press("ControlOrMeta+z");
      await expect(await cell(grid, "a", "qty")).toHaveText("1");
      await expectEdits(page, [{ cause: "undo", cells: [{ key: "a", column: "qty", before: 5, after: 1 }], removed: [] }], 3);

      await page.keyboard.press("ControlOrMeta+y");
      await expect(await cell(grid, "a", "qty")).toHaveText("5");
      await expectEdits(page, [{ cause: "redo", cells: [{ key: "a", column: "qty", before: 1, after: 5 }], removed: [] }], 4);
    });

    test("filling a 2x2 block", async ({ page }) => {
      const grid = page.locator("#grid");
      await (await cell(grid, "b", "qty")).click();
      await page.keyboard.press("Shift+ArrowDown");
      await page.keyboard.press("Shift+ArrowRight");
      await page.keyboard.type("7");
      await expect(grid.locator("input.wg-editor")).toBeFocused();
      await page.keyboard.press("ControlOrMeta+Enter");
      await expect(await cell(grid, "c", "price")).toHaveText("7");
      await expectEdits(page, [
        {
          cause: "fill",
          cells: [
            { key: "b", column: "qty", before: 2, after: 7 },
            { key: "b", column: "price", before: 20, after: 7 },
            { key: "c", column: "qty", before: 3, after: 7 },
            { key: "c", column: "price", before: 30, after: 7 },
          ],
          removed: [],
        },
      ]);
      // One step to undo.
      await page.keyboard.press("ControlOrMeta+z");
      await expect(await cell(grid, "c", "price")).toHaveText("30");
      const undone = await edits(page);
      expect(undone).toHaveLength(2);
      expect(undone[1].cause).toBe("undo");
      expect(undone[1].cells).toHaveLength(4);
    });

    test("pasting skips what does not read as the column's kind", async ({ page }) => {
      const grid = page.locator("#grid");
      await (await cell(grid, "c", "name")).click();
      await page.locator("#grid .wg-body").evaluate((body) => {
        const data = new DataTransfer();
        data.setData("text/plain", "Clementine\t9\nDurian\tlots\n");
        body.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }));
      });
      await expect(await cell(grid, "d", "name")).toHaveText("Durian");
      await expectEdits(page, [
        {
          cause: "paste",
          cells: [
            { key: "c", column: "name", before: "Cherry", after: "Clementine" },
            { key: "c", column: "qty", before: 3, after: 9 },
            { key: "d", column: "name", before: "Date", after: "Durian" },
          ],
          removed: [],
        },
      ]);
      await expect(await cell(grid, "d", "qty")).toHaveText("4");
      await expect.poll(() => page.evaluate(() => window.fixture.notes)).toContain("Pasted 3 cells; skipped 1 cell.");
    });

    test("deleting a selected row", async ({ page }) => {
      const grid = page.locator("#grid");
      await page.getByRole("rowheader", { name: "d", exact: true }).click();
      await expect.poll(() => page.evaluate(() => window.fixture.view.selectedKeys())).toEqual(["d"]);
      await page.evaluate(() => window.fixture.view.removeSelected());
      await expect(row(grid, "d")).toHaveCount(0);
      await expectEdits(page, [{ cause: "delete", cells: [], removed: ["d"] }]);
    });

    test("changes made from code are not reported", async ({ page }) => {
      const grid = page.locator("#grid");
      await page.evaluate(async () => {
        const { table } = window.fixture;
        await table.set("a", "qty", 42);
        await table.patch("b", { name: "Bilberry", price: 21 });
        await table.put("e", { name: "Elderberry", qty: 5, price: 50 });
        await table.drop("c");
      });
      await expect(await cell(grid, "a", "qty")).toHaveText("42");
      await expect(await cell(grid, "b", "name")).toHaveText("Bilberry");
      await expect(row(grid, "e")).toBeVisible();
      await expect(row(grid, "c")).toHaveCount(0);
      // Then a person's edit: only it is reported.
      await typeInto(grid, "a", "qty", "43");
      await expectEdits(page, [{ cause: "edit", cells: [{ key: "a", column: "qty", before: 42, after: 43 }], removed: [] }]);
      await page.evaluate(() => window.fixture.table.set("a", "qty", 44));
      await expect(await cell(grid, "a", "qty")).toHaveText("44");
      await page.waitForTimeout(200);
      expect(await edits(page)).toHaveLength(1);
    });
  });
}

function edits(page: Page): Promise<Edits[]> {
  return page.evaluate(() => window.fixture.edits);
}

/** That the edits reported from `from` on are exactly these, once they have all come. */
async function expectEdits(page: Page, expected: Edits[], from = 0) {
  await expect.poll(async () => (await edits(page)).length).toBe(from + expected.length);
  expect((await edits(page)).slice(from)).toEqual(expected);
}
