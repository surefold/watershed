// Every recipe in the Solid and React cookbooks, running against mocked
// services. The assertions are on what a person sees and on the requests
// made, not on how a recipe is written.

import type { Locator, Page } from "@playwright/test";
import { cell, row, typeInto } from "./grid";
import { PRODUCTS, STREAMED, WIKIS, expect, test } from "./mocks";

const COOKBOOKS = [
  // `unmount`: the page with the Effect recipe and a button that unmounts it (fixtures/unmount-*.tsx).
  { name: "Solid", path: "/grid/cookbook/", unmount: "/e2e/unmount-solid.html" },
  { name: "React", path: "/grid/react/cookbook/", unmount: "/e2e/unmount-react.html" },
];
const RECIPES = ["first-grid", "fetch", "paged", "save", "money", "websocket", "sse", "effect", "charts", "worker"];

/** A recipe's running example. */
const live = (page: Page, id: string) => page.locator(`section#${id} .live`);
const money = (n: number, digits = 2) =>
  new Intl.NumberFormat("en-US", { style: "currency", currency: "USD", minimumFractionDigits: digits, maximumFractionDigits: digits }).format(n);

for (const { name, path, unmount } of COOKBOOKS) {
  test.describe(`${name} cookbook`, () => {
    test.beforeEach(async ({ page }) => {
      await page.goto(path);
    });

    test("every recipe renders, with no errors", async ({ page, net }) => {
      for (const id of RECIPES) await expect(live(page, id), id).toBeVisible();
      // Each has a grid, but the worker's, which waits for its button.
      for (const id of RECIPES.filter((id) => id !== "worker")) await expect(live(page, id).locator(".wg-body"), id).toBeVisible();
      await expect(live(page, "worker").getByRole("button", { name: /Load 300,000 rows/ })).toBeEnabled();
      // The recipes that fetch on their own have their rows.
      await expect(row(live(page, "fetch"), "P-1")).toBeVisible();
      await expect(row(live(page, "paged"), "P-1")).toBeVisible();
      await expect(row(live(page, "save"), "1")).toBeVisible();
      // The streams connect only when asked.
      expect(net.coinbase.received).toEqual([]);
      // Let anything late go wrong before the errors are checked.
      await page.waitForTimeout(500);
    });

    test("first grid: the buttons change the total", async ({ page }) => {
      const recipe = live(page, "first-grid");
      // (2 × 24 + 38 + 6 × 7.5) × 1.2
      await expect(recipe).toContainText(`With tax: ${money(157.2)}`);
      await recipe.getByRole("button", { name: "One more kettle" }).click();
      await expect(recipe).toContainText(`With tax: ${money(186)}`);
      await expect(await cell(recipe, "o-1", "qty")).toHaveText("3");
      await recipe.getByRole("button", { name: "Tax to 25%" }).click();
      await expect(recipe).toContainText(`With tax: ${money(193.75)}`);
    });

    test("from an API: the fetched rows and their total", async ({ page, net }) => {
      const recipe = live(page, "fetch");
      // Sale price × stock: 3 × id × id for the discounted products, 4 × id × id for the others.
      const value = PRODUCTS.reduce((sum, p) => sum + p.price * (1 - p.discountPercentage / 100) * p.stock, 0);
      await expect(recipe).toContainText(`Stock value: ${money(value, 0)}`);
      await expect(await cell(recipe, "P-1", "title")).toHaveText("Product 1");
      await expect(await cell(recipe, "P-1", "price")).toHaveText("4.00");
      expect(net.api.lists.filter((url) => new URL(url).searchParams.get("limit") === "0")).toHaveLength(1);

      await recipe.getByRole("button", { name: "Fetch again" }).click();
      await expect.poll(() => net.api.lists.filter((url) => new URL(url).searchParams.get("limit") === "0").length).toBe(2);
      await expect(recipe.getByRole("button", { name: "Fetch again" })).toBeEnabled();
      // The same rows again, replaced rather than added.
      await expect(recipe).toContainText(`Stock value: ${money(value, 0)}`);
    });

    test("paged API: every page loads as the grid is scrolled", async ({ page, net }) => {
      const recipe = live(page, "paged");
      // The first page, and maybe the next: the grid asks for more while the rows end near the bottom of its view.
      await expect(recipe).toContainText(/(20|40) of 50 products loaded/);
      const body = recipe.locator(".wg-body");
      await expect(async () => {
        await body.evaluate((b) => (b.scrollTop = b.scrollHeight));
        await expect(recipe).toContainText("50 of 50 products loaded", { timeout: 1000 });
      }).toPass();
      await body.evaluate((b) => (b.scrollTop = b.scrollHeight));
      await expect(row(recipe, "P-50")).toBeVisible();
      const pages = net.api.lists.filter((url) => new URL(url).searchParams.get("limit") === "20").map((url) => new URL(url).searchParams.get("skip"));
      expect(new Set(pages)).toEqual(new Set(["0", "20", "40"]));
      // Nothing more is asked for once every page is in.
      const asked = net.api.lists.length;
      await body.evaluate((b) => (b.scrollTop = b.scrollHeight));
      await page.waitForTimeout(300);
      expect(net.api.lists).toHaveLength(asked);
    });

    test("paged API: a page that fails to load is asked for again", async ({ page, net }) => {
      // The browser logs the failed request.
      net.allow(/500/);
      // The second page fails until Try again has been clicked. A scroll near the
      // end asks for the page too, and can come at any moment: were the failures
      // simply turned off before the click, such a request could succeed first
      // and take the button away before it was clicked.
      const clicked = () => page.evaluate(() => (window as { triedAgain?: boolean }).triedAgain === true);
      let failed = 0;
      await page.route(/dummyjson\.com\/products\?.*skip=20/, async (route) => {
        if (route.request().method() === "GET" && !(await clicked())) {
          failed++;
          return route.fulfill({ status: 500, headers: { "access-control-allow-origin": "*" }, json: { message: "down" } });
        }
        return route.fallback();
      });
      await page.reload();
      // Noted on the way down to the button, before its own handler asks for the page.
      await page.evaluate(() =>
        document.addEventListener(
          "click",
          (e) => {
            if (e.target instanceof HTMLButtonElement && e.target.textContent === "Try again") {
              (window as { triedAgain?: boolean }).triedAgain = true;
            }
          },
          true,
        ),
      );
      const recipe = live(page, "paged");
      const body = recipe.locator(".wg-body");
      await expect(async () => {
        await body.evaluate((b) => (b.scrollTop = b.scrollHeight));
        await expect(recipe).toContainText("That page did not load", { timeout: 1000 });
      }).toPass();
      // The button shows once the failure has settled, with nothing asked for since.
      // Showing the failure changes the bar above the grid, which can move the grid
      // and give it one more scroll event near the end, and so one more request for
      // the page (which fails too). Wait until half a second has passed with none.
      const tryAgain = recipe.getByRole("button", { name: "Try again" });
      await expect
        .poll(async () => {
          const before = failed;
          await page.waitForTimeout(500);
          return failed === before && (await tryAgain.isVisible());
        })
        .toBe(true);
      expect(failed).toBeGreaterThanOrEqual(1);
      await expect(row(recipe, "P-21")).toHaveCount(0);
      // A list at its end cannot be scrolled further, so the recipe offers to try again.
      await tryAgain.click();
      await expect(tryAgain).toHaveCount(0);
      await expect(row(recipe, "P-21")).toBeAttached();
      await expect(async () => {
        await body.evaluate((b) => (b.scrollTop = 0));
        await body.evaluate((b) => (b.scrollTop = b.scrollHeight));
        await expect(recipe).toContainText("50 of 50 products loaded", { timeout: 1000 });
      }).toPass();
    });

    test.describe("edit and save", () => {
      test("one PATCH a changed row, with only its changed fields; undo makes a row clean", async ({ page, net }) => {
        const recipe = live(page, "save");
        await expect(row(recipe, "1")).toBeVisible();
        await typeInto(recipe, "1", "price", "99");
        await typeInto(recipe, "1", "stock", "5");
        await typeInto(recipe, "2", "title", "Renamed");
        await typeInto(recipe, "3", "price", "1");
        await expect(save(recipe, 3)).toBeEnabled();
        // Undone: row 3 is as the server has it, with nothing to save.
        await page.keyboard.press("ControlOrMeta+z");
        await expect(await cell(recipe, "3", "price")).toHaveText("12.00");
        await expect(save(recipe, 2)).toBeEnabled();
        // The rows with something to save are marked (rowClass), and only they.
        await expect(row(recipe, "1")).toHaveClass(/\bunsaved\b/);
        await expect(row(recipe, "2")).toHaveClass(/\bunsaved\b/);
        await expect(row(recipe, "3")).not.toHaveClass(/\bunsaved\b/);

        await save(recipe, 2).click();
        await expect(save(recipe, 0)).toBeDisabled();
        await expect(recipe.locator(".wg-row.unsaved")).toHaveCount(0);
        expect(byId(net.api.patches)).toEqual([
          { id: "1", body: { price: 99, stock: 5 }, status: 200 },
          { id: "2", body: { title: "Renamed" }, status: 200 },
        ]);
        await expect(recipe).toContainText("PATCH /products/1");
        await expect(recipe).toContainText("saved");
      });

      test("a failed save stays unsaved, and saves when tried again", async ({ page, net }) => {
        // The browser logs the failed request.
        net.allow(/503/);
        net.api.failing.add("4");
        const recipe = live(page, "save");
        await expect(row(recipe, "1")).toBeVisible();
        await typeInto(recipe, "4", "price", "123");
        await save(recipe, 1).click();
        await expect(recipe).toContainText("failed");
        await expect(save(recipe, 1)).toBeEnabled();
        expect(net.api.patches).toEqual([{ id: "4", body: { price: 123 }, status: 503 }]);

        net.api.failing.delete("4");
        await save(recipe, 1).click();
        await expect(save(recipe, 0)).toBeDisabled();
        expect(net.api.patches).toEqual([
          { id: "4", body: { price: 123 }, status: 503 },
          { id: "4", body: { price: 123 }, status: 200 },
        ]);
        await expect(await cell(recipe, "4", "price")).toHaveText("123.00");
      });

      test("Discard puts the server's values back", async ({ page, net }) => {
        const recipe = live(page, "save");
        await expect(row(recipe, "1")).toBeVisible();
        await typeInto(recipe, "5", "title", "Gone");
        await typeInto(recipe, "6", "stock", "0");
        await expect(save(recipe, 2)).toBeEnabled();
        await recipe.getByRole("button", { name: "Discard" }).click();
        await expect(await cell(recipe, "5", "title")).toHaveText("Product 5");
        await expect(await cell(recipe, "6", "stock")).toHaveText("6");
        await expect(save(recipe, 0)).toBeDisabled();
        await expect(recipe.getByRole("button", { name: "Discard" })).toBeDisabled();
        expect(net.api.patches).toEqual([]);
      });
    });

    test("money: decimals add up to the cent, and ordinary numbers do not", async ({ page }) => {
      const recipe = live(page, "money");
      // Each line's gross is net plus VAT at 20%, to the cent: 5.28, 0.36, 2.28, 8.16, 4.20, 2.88.
      await expect(recipe).toContainText("To pay: €23.16");
      await expect(recipe).toContainText("As numbers: 23.159999999999997");
      await expect(await cell(recipe, "L1", "vat")).toHaveText("0.88");
      await expect(await cell(recipe, "L1", "gross")).toHaveText("5.28");
      // A quantity typed in is read as a decimal: 2 × 0.10, with VAT 0.04.
      await typeInto(recipe, "L2", "qty", "2");
      await expect(await cell(recipe, "L2", "gross")).toHaveText("0.24");
      await expect(recipe).toContainText("To pay: €23.04");
      // A cookie at 0.70: 0.84 with VAT.
      await recipe.getByRole("button", { name: "Add a cookie" }).click();
      await expect(await cell(recipe, "L7", "gross")).toHaveText("0.84");
      await expect(recipe).toContainText("To pay: €23.88");
      const asNumbers = (await recipe.locator(".bar").textContent())?.match(/As numbers: (\S+)/)?.[1];
      expect(Number(asNumbers)).toBeCloseTo(23.88, 10);
    });

    test("WebSocket: rows and the gain follow the ticks", async ({ page, net }) => {
      const recipe = live(page, "websocket");
      await recipe.getByRole("button", { name: "Connect" }).click();
      const subscription = await net.coinbase.subscribed;
      expect(subscription.channels).toEqual(["ticker"]);
      expect(subscription.product_ids).toEqual(expect.arrayContaining(["BTC-USD", "ETH-USD"]));
      await expect(recipe.getByRole("button", { name: "Disconnect" })).toBeVisible();

      net.coinbase.send({ type: "heartbeat", product_id: "BTC-USD" }); // not a price: passed over
      net.coinbase.tick([
        ["BTC-USD", 50000],
        ["ETH-USD", 3000],
      ]);
      await expect(row(recipe, "BTC-USD")).toBeVisible();
      await expect(row(recipe, "ETH-USD")).toBeVisible();
      await expect(recipe).toContainText("2 messages");
      await expect(recipe).toContainText(`Gain since connecting: ${money(0)}`);

      net.coinbase.tick([
        ["BTC-USD", 50100],
        ["ETH-USD", 2990],
      ]);
      // 0.5 × 100 + 8 × -10
      await expect(recipe).toContainText(`Gain since connecting: ${money(-30)}`);
      await expect(recipe).toContainText("4 messages");
      await expect(await cell(recipe, "BTC-USD", "price")).toContainText("50,100");
      await expect(await cell(recipe, "BTC-USD", "open")).toContainText("50,000");

      await recipe.getByRole("button", { name: "Disconnect" }).click();
      await expect(recipe.getByRole("button", { name: "Connect" })).toBeVisible();
    });

    test("SSE: edits grouped by wiki, keeping the latest 2,000", async ({ page }) => {
      const recipe = live(page, "sse");
      await recipe.getByRole("button", { name: "Connect" }).click();
      // The canary and the log events are passed over.
      await expect(recipe).toContainText(`2,000 edits on ${WIKIS.length} wikis`);
      // The latest 2,000 of the edits sent, by wiki.
      const kept = Array.from({ length: 2000 }, (_, k) => STREAMED - 2000 + k);
      for (const [at, wiki] of WIKIS.entries()) {
        const count = kept.filter((k) => k % WIKIS.length === at).length;
        await expect(recipe.locator(".wg-row.wg-group").filter({ hasText: wiki })).toContainText(count.toLocaleString("en-US"));
      }
      await expect(recipe.locator(".wg-row.wg-group")).toHaveCount(WIKIS.length);
      await recipe.getByRole("button", { name: "Disconnect" }).click();
      await expect(recipe.getByRole("button", { name: "Connect" })).toBeVisible();
    });

    test.describe("Effect", () => {
      // The recipe waits 0.5 s, give or take 20%, before its first try again, then 1 s, give or take 20%.
      const FIRST_WAIT = /Reconnecting in 0\.[4-6] s \(attempt 1\)/;
      const SECOND_WAIT = /Reconnecting in (0\.[89]|1\.[0-2]) s \(attempt 2\)/;
      /** Longer than any first wait: a connection still to come would have come by then. */
      const PAST_A_WAIT = 1500;

      test("rows and the gain follow the ticks", async ({ page, net }) => {
        const recipe = live(page, "effect");
        await recipe.getByRole("button", { name: "Connect" }).click();
        const subscription = await net.coinbase.subscribed;
        expect(subscription.channels).toEqual(["ticker"]);
        expect(subscription.product_ids).toEqual(expect.arrayContaining(["BTC-USD", "ETH-USD"]));
        await expect(recipe).toContainText("Live");

        net.coinbase.send({ type: "heartbeat", product_id: "BTC-USD" }); // not a price: passed over
        net.coinbase.tick([
          ["BTC-USD", 50000],
          ["ETH-USD", 3000],
        ]);
        await expect(row(recipe, "BTC-USD")).toBeVisible();
        await expect(row(recipe, "ETH-USD")).toBeVisible();
        await expect(recipe).toContainText("2 messages");
        await expect(recipe).toContainText(`Gain since connecting: ${money(0)}`);

        // A burst: only the latest price of each coin counts.
        net.coinbase.tick([
          ["BTC-USD", 50050],
          ["ETH-USD", 2995],
          ["BTC-USD", 50100],
          ["ETH-USD", 2990],
        ]);
        // 0.5 × 100 + 8 × -10
        await expect(recipe).toContainText(`Gain since connecting: ${money(-30)}`);
        await expect(recipe).toContainText("6 messages");
        await expect(await cell(recipe, "BTC-USD", "price")).toContainText("50,100");
        await expect(await cell(recipe, "BTC-USD", "open")).toContainText("50,000");
        // The WebSocket recipe's table is its own.
        await expect(row(live(page, "websocket"), "BTC-USD")).toHaveCount(0);
      });

      test("a dropped connection is opened again after a backoff, and subscribes again", async ({ page, net }) => {
        const recipe = live(page, "effect");
        await recipe.getByRole("button", { name: "Connect" }).click();
        await expect.poll(() => net.coinbase.subscriptions).toBe(1);
        await expect(recipe).toContainText("Live");
        net.coinbase.tick([["BTC-USD", 50000]]);
        await expect(row(recipe, "BTC-USD")).toBeVisible();

        await net.coinbase.drop();
        await expect(recipe).toContainText(FIRST_WAIT);
        await expect.poll(() => net.coinbase.subscriptions).toBe(2);
        expect(net.coinbase.connections).toBe(2);
        await expect(recipe).toContainText("Live");
        // The prices flow again, into the same rows.
        net.coinbase.tick([["BTC-USD", 50100]]);
        await expect(recipe).toContainText(`Gain since connecting: ${money(50)}`);
        await expect(recipe).toContainText("2 messages");

        // While the server is down, each wait is longer than the one before.
        net.coinbase.refusing = true;
        await net.coinbase.drop();
        await expect(recipe).toContainText(FIRST_WAIT);
        await expect(recipe).toContainText(SECOND_WAIT);
        expect(net.coinbase.connections).toBe(3);
        net.coinbase.refusing = false;
        await expect.poll(() => net.coinbase.subscriptions).toBe(3);
        expect(net.coinbase.connections).toBe(4);
        await expect(recipe).toContainText("Live");

        // A connection that worked starts the backoff again from the first wait.
        await net.coinbase.drop();
        await expect(recipe).toContainText(FIRST_WAIT);
        await expect.poll(() => net.coinbase.subscriptions).toBe(4);
        net.coinbase.tick([["BTC-USD", 50200]]);
        await expect(recipe).toContainText(`Gain since connecting: ${money(100)}`);
      });

      test("Disconnect closes the socket, and stops a reconnect", async ({ page, net }) => {
        const recipe = live(page, "effect");
        await recipe.getByRole("button", { name: "Connect" }).click();
        await expect.poll(() => net.coinbase.subscriptions).toBe(1);
        await recipe.getByRole("button", { name: "Disconnect" }).click();
        await expect.poll(() => net.coinbase.open).toBe(false);
        await expect(recipe).toContainText("Not connected");
        await expect(recipe.getByRole("button", { name: "Connect" })).toBeVisible();

        // Disconnected while waiting to reconnect: no more tries.
        await recipe.getByRole("button", { name: "Connect" }).click();
        await expect.poll(() => net.coinbase.subscriptions).toBe(2);
        await net.coinbase.drop();
        await expect(recipe).toContainText(FIRST_WAIT);
        await recipe.getByRole("button", { name: "Disconnect" }).click();
        await page.waitForTimeout(PAST_A_WAIT);
        expect(net.coinbase.connections).toBe(2);
        await expect(recipe).toContainText("Not connected");
      });

      test("unmounting closes the socket, and stops a reconnect", async ({ page, net }) => {
        await page.goto(unmount);
        const recipe = page.locator("#recipe");
        await recipe.getByRole("button", { name: "Connect" }).click();
        await expect.poll(() => net.coinbase.subscriptions).toBe(1);
        net.coinbase.tick([["BTC-USD", 50000]]);
        await expect(row(recipe, "BTC-USD")).toBeVisible();
        await page.getByRole("button", { name: "Unmount" }).click();
        await expect(page.getByText("Unmounted")).toBeVisible();
        await expect.poll(() => net.coinbase.open).toBe(false);

        // Unmounted while waiting to reconnect: no more tries.
        await page.reload();
        await recipe.getByRole("button", { name: "Connect" }).click();
        await expect.poll(() => net.coinbase.subscriptions).toBe(2);
        await net.coinbase.drop();
        await expect(recipe).toContainText(FIRST_WAIT);
        await page.getByRole("button", { name: "Unmount" }).click();
        await page.waitForTimeout(PAST_A_WAIT);
        expect(net.coinbase.connections).toBe(2);
      });

      test("leaving the page while waiting to reconnect makes no more connections", async ({ page, net }) => {
        const recipe = live(page, "effect");
        await recipe.getByRole("button", { name: "Connect" }).click();
        await expect.poll(() => net.coinbase.subscriptions).toBe(1);
        await net.coinbase.drop();
        await expect(recipe).toContainText(FIRST_WAIT);
        await page.goto(COOKBOOKS.find((other) => other.path !== path)!.path);
        await page.waitForTimeout(PAST_A_WAIT);
        expect(net.coinbase.connections).toBe(1);
      });
    });

    test("charts: the bars follow the table", async ({ page }) => {
      const recipe = live(page, "charts");
      const bars = recipe.getByRole("img", { name: "Stock value by category" });
      const widths = () => bars.locator("rect").evaluateAll((rects) => rects.map((r) => r.getAttribute("width")));
      // furniture 340 × 12 + 85 × 40, lighting 42 × 75 + 120 × 18, textiles 210 × 9 + 24 × 120
      await expect(bars).toContainText(`furniture · ${money(7480, 0)}`);
      await expect(bars).toContainText(`lighting · ${money(5310, 0)}`);
      await expect(bars).toContainText(`textiles · ${money(4770, 0)}`);
      const chart = recipe.locator(".gc-line").locator("..");
      const drawn = await chart.textContent();
      const before = await widths();

      await recipe.getByRole("button", { name: "Restock cushions" }).click();
      await expect(bars).toContainText(`textiles · ${money(7170, 0)}`);
      await recipe.getByRole("button", { name: "Desk on sale" }).click();
      await expect(bars).toContainText(`furniture · ${money(6880, 0)}`);
      expect(await widths()).not.toEqual(before);
      // The package's own chart is drawn again too.
      await expect(chart).not.toHaveText(drawn ?? "");
    });

    test("in a worker: loads its rows, and moving prices recalculates", async ({ page }) => {
      test.slow();
      const recipe = live(page, "worker");
      await recipe.getByRole("button", { name: /Load 300,000 rows/ }).click();
      await expect(row(recipe, "T-0")).toBeVisible();
      // Until every block is in, the recipe says how far it has got.
      await expect(recipe.getByText(/Starting the worker|Loaded [\d,]+ of 300,000/)).toHaveCount(0, { timeout: 90_000 });
      // Every row is in: qty is 100 × (1 + k % 20) for trade k, so 300,000 trades add up to 315,000,000.
      await expect(recipe.locator(".wg-total")).toContainText("315,000,000");
      const figure = recipe.getByText(/last update: [\d,]+ nodes/);
      await expect(figure).toBeVisible();
      const loaded = await figure.textContent();
      const pnl = await recipe.getByText(/P&L/).textContent();

      await recipe.getByRole("button", { name: "Move 1,000 prices" }).click();
      await expect(figure).not.toHaveText(loaded ?? "");
      const nodes = Number(/last update: ([\d,]+) nodes/.exec((await figure.textContent()) ?? "")?.[1].replace(/,/g, ""));
      expect(nodes).toBeGreaterThan(0);
      await expect(recipe.getByText(/P&L/)).not.toHaveText(pnl ?? "");
    });
  });
}

/** The save button, when it offers to save this many changes. */
function save(recipe: Locator, changes: number) {
  return recipe.getByRole("button", { name: `Save ${changes} change${changes === 1 ? "" : "s"}` });
}

function byId<T extends { id: string }>(list: T[]): T[] {
  return [...list].sort((a, b) => Number(a.id) - Number(b.id));
}
