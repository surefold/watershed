// Decimal columns: exact in the engine, exact on the way to the page, and
// shown exactly, with a Table and a WorkerTable.

import { expect, test } from "@playwright/test";
import { cell, columnIndex, row, typeInto } from "./grid";
import { watchErrors } from "./mocks";

for (const kind of ["table", "worker"] as const) {
  test.describe(`decimal columns with a ${kind === "table" ? "Table" : "WorkerTable"}`, () => {
    let errors: string[];

    test.beforeEach(async ({ page }) => {
      errors = watchErrors(page);
      await page.goto(`/e2e/decimal.html?table=${kind}`);
      await expect(page.locator("body")).toHaveAttribute("data-ready", kind);
      await expect(row(page.locator("#grid"), "p-3")).toBeVisible();
    });

    test.afterEach(async ({ page }) => {
      expect(await page.evaluate(() => window.decimals.errors), "the grid's onError").toEqual([]);
      expect(errors, "page and console errors").toEqual([]);
    });

    test("cells and totals come back exactly, as text", async ({ page }) => {
      const read = await page.evaluate(async () => {
        const t = window.decimals.table;
        // A WorkerTable's `get` asks the worker; a Table's answers at once.
        const before = { row: await t.get("p-1"), exact: t.totalExact("amount"), total: t.total("amount") };
        // 0.10 + 0.20 - 0.30 is 0 exactly; as JavaScript numbers it is 5.55e-17.
        await t.drop("p-3");
        await t.put("p-4", { desk: "FX", amount: "-0.30" });
        return { ...before, after: t.totalExact("amount"), fee: t.totalExact("fee") };
      });
      expect(read.row).toEqual({ desk: "Rates", amount: "0.10", fee: "0.00150" });
      expect(read.exact).toBe("12345678901234568.19");
      // The nearest number, for code that wants one.
      expect(read.total).toBe(12345678901234568);
      expect(read.after).toBe("0.00");
      expect(read.fee).toBe("0.00000");
    });

    test("the grid shows a decimal exactly, and what is typed is read exactly", async ({ page }) => {
      const grid = page.locator("#grid");
      const total = grid.locator(".wg-foot .wg-cell").nth(await columnIndex(grid, "amount"));
      // Past what a JavaScript number holds: 17 digits before the point and two after.
      await expect(await cell(grid, "p-3", "amount")).toHaveText("12,345,678,901,234,567.89");
      await expect(total).toHaveText("12,345,678,901,234,568.19");
      await expect(await cell(grid, "p-1", "fee")).toHaveText("0.0015");
      await typeInto(grid, "p-1", "amount", "0.3");
      await expect(total).toHaveText("12,345,678,901,234,568.39");
      expect(await page.evaluate(async () => (await window.decimals.table.get("p-1"))?.amount)).toBe("0.3");
      // A decimal column filters and sorts like a number column.
      await page.evaluate(async () => await window.decimals.table.filter("amount", "<1"));
      await expect(row(grid, "p-3")).toHaveCount(0);
      await expect(total).toHaveText("0.50");
    });
  });
}
