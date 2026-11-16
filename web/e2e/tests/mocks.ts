// Every request a page makes off the test server is answered here or refused,
// so the tests never touch the network: DummyJSON's products, Coinbase's
// ticker WebSocket (which a test can drop, as a server would), Wikimedia's
// stream of edits (SSE) and Google Fonts. Anything else is refused and fails
// the test, as do page and console errors.

import { test as base, expect, type Page, type Route, type WebSocketRoute } from "@playwright/test";

/** Page errors and console errors from now on. */
export function watchErrors(page: Page): string[] {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(`page error: ${error.message}`));
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(`console error: ${message.text()}`);
  });
  return errors;
}

// ------------------------------------------------------------------ DummyJSON

export interface Product {
  id: number;
  title: string;
  category: string;
  price: number;
  stock: number;
  discountPercentage: number;
}

const CATEGORIES = ["beauty", "fragrances", "furniture"];

/** 50 products, with numbers chosen so sums come out whole: a sale price of 3 × id when discounted. */
export const PRODUCTS: Product[] = Array.from({ length: 50 }, (_, k) => {
  const id = k + 1;
  return { id, title: `Product ${id}`, category: CATEGORIES[id % 3], price: 4 * id, stock: id, discountPercentage: id % 2 ? 25 : 0 };
});

export interface Patch {
  id: string;
  body: Record<string, unknown>;
  status: number;
}

const CORS = {
  "access-control-allow-origin": "*",
  "access-control-allow-methods": "GET, PATCH, OPTIONS",
  "access-control-allow-headers": "content-type",
};

async function dummyJson(route: Route, api: Network["api"]) {
  const request = route.request();
  const url = new URL(request.url());
  if (request.method() === "OPTIONS") return route.fulfill({ status: 204, headers: CORS });
  if (request.method() === "GET" && url.pathname === "/products") {
    api.lists.push(url.toString());
    // As DummyJSON: 30 by default, 0 for all of them, from `skip`.
    const limit = Number(url.searchParams.get("limit") ?? 30);
    const skip = Number(url.searchParams.get("skip") ?? 0);
    const products = PRODUCTS.slice(skip, limit === 0 ? undefined : skip + limit);
    return route.fulfill({ headers: CORS, json: { products, total: PRODUCTS.length, skip, limit: products.length } });
  }
  const one = /^\/products\/(\d+)$/.exec(url.pathname);
  if (request.method() === "PATCH" && one) {
    const id = one[1];
    const body = request.postDataJSON() as Record<string, unknown>;
    const status = api.failing.has(id) ? 503 : 200;
    api.patches.push({ id, body, status });
    if (status !== 200) return route.fulfill({ status, headers: CORS, json: { message: "Service Unavailable" } });
    const product = PRODUCTS.find((p) => String(p.id) === id);
    return route.fulfill({ headers: CORS, json: { ...product, ...body } });
  }
  return route.fulfill({ status: 404, headers: CORS, json: { message: `No mock for ${request.method()} ${url.pathname}` } });
}

// ----------------------------------------------------------------- Wikimedia

export const WIKIS = ["en.wikipedia.org", "de.wikipedia.org", "commons.wikimedia.org"];
/** How many edits the stream sends; the recipe keeps the latest 2,000. */
export const STREAMED = 2500;

/** Wikimedia's recent changes, as one response of server-sent events, with some the recipe should pass over. */
function recentChanges(): string {
  // Wait an hour before reconnecting once this response ends, so the same edits do not come twice.
  const events = ["retry: 3600000\n\n"];
  const send = (data: object) => events.push(`data: ${JSON.stringify(data)}\n\n`);
  for (let k = 0; k < STREAMED; k++) {
    send({
      type: "edit",
      server_name: WIKIS[k % WIKIS.length],
      title: `Page ${k}`,
      bot: k % 5 === 0,
      length: { old: 100, new: 100 + (k % 7) - 3 },
      meta: { id: `edit-${k}`, domain: WIKIS[k % WIKIS.length] },
    });
    if (k % 500 === 0) {
      // Not edits, and a canary: none of these should show.
      send({ type: "log", server_name: "meta.wikimedia.org", title: "A log", bot: false, meta: { id: `log-${k}`, domain: "meta.wikimedia.org" } });
      send({ type: "edit", server_name: "canary.wikipedia.org", title: "Canary", bot: true, meta: { id: `canary-${k}`, domain: "canary" } });
    }
  }
  return events.join("");
}

// ------------------------------------------------------------------ Coinbase

export interface Coinbase {
  /** The messages the page sent, parsed. */
  received: Record<string, unknown>[];
  /** Resolves once the page has subscribed. */
  subscribed: Promise<{ product_ids: string[]; channels: string[] }>;
  /** Sends ticker messages, `[coin, price]` each, to the page. */
  tick(prices: [string, number][]): void;
  /** Sends any message to the page. */
  send(message: object): void;
  /** How many times the page has opened the WebSocket. */
  readonly connections: number;
  /** How many subscribe messages the page has sent, over every connection. */
  readonly subscriptions: number;
  /** Whether the latest connection is still open: neither the page nor the server has closed it. */
  readonly open: boolean;
  /** Closes the latest connection from the server's side, as a dropped connection. */
  drop(): Promise<void>;
  /** While true, each new connection is closed as soon as it opens, as by a server that is down. */
  refusing: boolean;
}

function coinbase(): Coinbase & { connect(ws: WebSocketRoute): void } {
  let socket: WebSocketRoute | undefined;
  let open = false;
  let connections = 0;
  let subscriptions = 0;
  let subscribe!: (message: { product_ids: string[]; channels: string[] }) => void;
  const received: Record<string, unknown>[] = [];
  const subscribed = new Promise<{ product_ids: string[]; channels: string[] }>((resolve) => (subscribe = resolve));
  let sequence = 1;
  const send = (message: object) => {
    if (!socket) throw new Error("The page has not opened the WebSocket");
    socket.send(JSON.stringify(message));
  };
  return {
    refusing: false,
    connect(ws) {
      connections++;
      if (this.refusing) return void ws.close({ code: 1013, reason: "Try again later" });
      socket = ws; // not connected to the real server
      open = true;
      ws.onMessage((text) => {
        const message = JSON.parse(String(text)) as Record<string, unknown>;
        received.push(message);
        if (message.type === "subscribe") {
          subscriptions++;
          ws.send(JSON.stringify({ type: "subscriptions", channels: [{ name: "ticker", product_ids: message.product_ids }] }));
          subscribe(message as { product_ids: string[]; channels: string[] });
        }
      });
      // The page closed it.
      ws.onClose(() => {
        if (ws === socket) open = false;
      });
    },
    received,
    subscribed,
    send,
    get connections() {
      return connections;
    },
    get subscriptions() {
      return subscriptions;
    },
    get open() {
      return open;
    },
    async drop() {
      if (!socket || !open) throw new Error("No open WebSocket to drop");
      open = false;
      await socket.close({ code: 1011, reason: "Dropped by the test" });
    },
    tick(prices) {
      for (const [coin, price] of prices) {
        send({ type: "ticker", sequence: sequence++, product_id: coin, price: String(price), time: new Date().toISOString() });
      }
    },
  };
}

// ------------------------------------------------------------------ together

export interface Network {
  api: {
    /** The URLs of each GET of the product list. */
    lists: string[];
    /** Each PATCH, with its body and the status it was answered with. */
    patches: Patch[];
    /** Product ids whose PATCH is answered with a 503 until taken out. */
    failing: Set<string>;
  };
  coinbase: Coinbase;
  /** Requests to anywhere not mocked, refused. */
  unexpected: string[];
  /** Page and console errors. */
  errors: string[];
  /** Lets console errors matching this through, for a test that makes one happen. */
  allow(pattern: RegExp): void;
}

export async function mockNetwork(page: Page): Promise<Network> {
  const allowed: RegExp[] = [];
  const seen = watchErrors(page);
  const feed = coinbase();
  const net: Network = {
    api: { lists: [], patches: [], failing: new Set() },
    coinbase: feed,
    unexpected: [],
    get errors() {
      return seen.filter((error) => !allowed.some((pattern) => pattern.test(error)));
    },
    allow: (pattern) => void allowed.push(pattern),
  };
  await page.route(
    (url) => url.hostname !== "127.0.0.1" && url.hostname !== "localhost",
    async (route) => {
      const url = new URL(route.request().url());
      switch (url.hostname) {
        case "fonts.googleapis.com":
          return route.fulfill({ contentType: "text/css", body: "" });
        case "dummyjson.com":
          return dummyJson(route, net.api);
        case "stream.wikimedia.org":
          if (url.pathname === "/v2/stream/recentchange") {
            return route.fulfill({ headers: { ...CORS, "cache-control": "no-cache" }, contentType: "text/event-stream", body: recentChanges() });
          }
      }
      net.unexpected.push(`${route.request().method()} ${url}`);
      return route.abort();
    },
  );
  // Every WebSocket: Coinbase's is mocked (whatever its path), and any other is refused.
  await page.routeWebSocket(
    () => true,
    (ws) => {
      const url = new URL(ws.url());
      if (url.hostname === "ws-feed.exchange.coinbase.com") return feed.connect(ws);
      net.unexpected.push(`WebSocket ${url}`);
      void ws.close({ code: 1008, reason: "Not mocked" });
    },
  );
  return net;
}

/** Playwright's `test`, with every page's network mocked and its errors checked when each test ends. */
export const test = base.extend<{ net: Network }>({
  net: [
    async ({ page }, use) => {
      const net = await mockNetwork(page);
      await use(net);
      expect(net.unexpected, "requests off the test server that nothing mocks").toEqual([]);
      expect(net.errors, "page and console errors").toEqual([]);
    },
    { auto: true },
  ],
});

export { expect };
