// Draws a watershed table as a grid. The rows, calculated columns, totals,
// filtering, sorting and grouping all live in the table (WebAssembly); this
// file only draws the lines in view and passes on what the person does.
//
//   const view = drawGrid(element, table, { keyColumn: { title: "trade" } });
//   table.set("T-000001", "qty", 500);   // the view redraws by itself
//   view.setTheme("dark");                // one of themes (see grid.css)
//
// A column is known by its position in the engine (0, 1, 2…). On screen the
// key column comes first, then pinned columns, then the rest, in whatever
// order the person has dragged them into. A cell's `pos` is 0 for the key
// column and the column's position plus one otherwise.

const OVERSCAN = 6;
const TREND_POINTS = 32;
const SVG = "http://www.w3.org/2000/svg";

/** A cell as the engine gives it, as the table's `Cell`: an error is `{ error }`. */
function cellOf(value) {
  return value !== null && typeof value === "object" ? { error: String(value.e) } : value;
}

/** The engine gives JSON as text; a WorkerTable's engine gives it parsed. */
function json(value) {
  return typeof value === "string" ? JSON.parse(value) : value;
}

/** Whether a column of this kind holds numbers: a number or a decimal. */
function numeric(kind) {
  return kind === "number" || kind === "decimal";
}

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

/** The themes grid.css has, for a list to choose from. "" takes the page's colours. */
export const themes = [
  { id: "", name: "Match the page" },
  { id: "light", name: "Light" },
  { id: "dark", name: "Dark" },
  { id: "paper", name: "Paper" },
  { id: "dusk", name: "Dusk" },
  { id: "terminal", name: "Terminal" },
  { id: "contrast", name: "High contrast" },
];

/** Draws `table` into `root` (an element with a height). See `GridOptions` in index.d.ts. */
export function drawGrid(root, table, options) {
  return new GridView(root, table, options);
}

export class GridView {

  constructor(root, table, options = {}) {
    this.root = root;
    this.table = table;
    // A table in a worker gives each view an engine that reads what it draws
    // from the worker's snapshots, and draws again when rows it asked for come.
    this.grid = typeof table.connect === "function" ? table.connect(() => this.draw()) : table.engine;
    this.epoch = 0;
    this.rowH = options.rowHeight ?? 32;
    this.keyTitle = options.keyColumn?.title ?? "key";
    this.keyWidth = options.keyColumn?.width ?? 176;
    this.label = options.label ?? "Data grid";
    this.theme = options.theme ?? "";
    this.formats = options.columns ?? {};
    this.loadMore = options.loadMore ?? null;
    this.rowClass = options.rowClass ?? null;
    this.onStatus = options.onStatus ?? (() => {});
    this.onFocus = options.onFocus ?? (() => {});
    this.onGroup = options.onGroup ?? (() => {});
    this.onNote = options.onNote ?? (() => {});
    this.onEdit = options.onEdit ?? null;
    this.onError = options.onError ?? ((message) => console.warn(message));

    this.cols = [];
    this.order = []; // every column, in the order the person wants
    this.hidden = new Set();
    this.pinned = new Set();
    this.display = [0]; // the positions on screen, left to right
    this.widths = []; // by position
    this.sort = []; // [{ col, desc }]
    this.groupCol = -1;
    this.selected = new Set(); // row keys
    this.anchor = null; // the line a shift-click selects from
    // A block of cells picked out by dragging or Shift+arrows: from the focused
    // cell to (toLine, toAcross), where `across` counts columns along the screen.
    this.range = null; // { line, across, toLine, toAcross }
    this.focus = null; // { line, pos, kind: "cell" | "group" | "total", key, col }
    this.live = new Map(); // line number → its row element
    this.spare = [];
    this.before = new Map(); // what each visible line showed last time, to flash changes
    this.editing = null;
    this.menu = null;
    this.viewLen = -1;
    this.frame = 0;
    this.loading = false;
    this.done = false;
    this.destroyed = false;

    this.build();
    this.readColumns();
    // The table may be sorted, filtered or grouped already.
    this.followView();
    // Whatever changes the table, here or anywhere else, redraws the view.
    this.unwatch = table.watch((change) => this.redraw(change));
    this.refresh();
  }

  // ------------------------------------------------------------- structure

  build() {
    this.root.classList.add("wg");
    this.setTheme(this.theme);
    this.root.style.setProperty("--wg-row", `${this.rowH}px`);
    this.root.replaceChildren();
    this.head = el("div", "wg-head");
    this.headRow = el("div", "wg-hrow");
    this.filterRow = el("div", "wg-frow");
    this.head.append(this.headRow, this.filterRow);
    this.body = el("div", "wg-body");
    this.body.tabIndex = 0;
    this.body.setAttribute("role", "grid");
    this.spacer = el("div", "wg-spacer");
    this.rows = el("div", "wg-rows");
    this.empty = el("div", "wg-empty", "No rows match.");
    this.empty.hidden = true;
    this.body.append(this.spacer, this.rows, this.empty);
    this.foot = el("div", "wg-foot");
    this.footRow = el("div", "wg-row wg-total");
    this.foot.append(this.footRow);
    this.root.append(this.head, this.body, this.foot);

    this.body.addEventListener("scroll", () => {
      this.head.scrollLeft = this.body.scrollLeft;
      this.foot.scrollLeft = this.body.scrollLeft;
      this.root.classList.toggle("wg-scrolled", this.body.scrollTop > 0);
      this.root.classList.toggle("wg-scrolled-x", this.body.scrollLeft > 0);
      this.schedule();
      if (this.nearEnd()) this.fetchMore();
    });
    this.rows.addEventListener("click", (e) => this.clicked(e));
    this.rows.addEventListener("pointerdown", (e) => this.dragRange(e));
    this.said = el("div", "wg-said");
    this.said.setAttribute("aria-live", "polite");
    this.root.append(this.said);
    this.body.setAttribute("aria-label", this.label);
    this.body.setAttribute("aria-multiselectable", "true");
    this.rows.addEventListener("dblclick", (e) => {
      const hit = this.hit(e);
      if (hit && hit.data.k !== undefined) this.edit(hit.line, hit.pos);
    });
    this.body.addEventListener("keydown", (e) => this.key(e));
    this.body.addEventListener("copy", (e) => this.copy(e));
    this.body.addEventListener("paste", (e) => this.paste(e));
    this.footRow.addEventListener("click", (e) => {
      const cell = e.target.closest(".wg-cell");
      if (!cell || cell._pos === 0) return;
      this.setFocus({ line: -1, pos: cell._pos, kind: "total", key: null, col: cell._pos - 1 });
      this.draw();
    });
    this.observer = new ResizeObserver(() => this.schedule());
    this.observer.observe(this.body);
    this.outside = (e) => {
      if (this.menu && !this.menu.contains(e.target) && !e.target.closest?.(".wg-more")) this.closeMenu();
    };
    this.escape = (e) => e.key === "Escape" && this.closeMenu();
  }

  /** Changes how the grid looks: a theme's name, or "" for the page's colours. */
  setTheme(theme) {
    this.theme = theme || "";
    if (this.theme) this.root.dataset.wgTheme = this.theme;
    else delete this.root.dataset.wgTheme;
  }

  destroy() {
    if (this.destroyed) return;
    this.destroyed = true;
    this.unwatch();
    this.grid.disconnect?.();
    this.closeMenu();
    this.observer.disconnect();
    cancelAnimationFrame(this.frame);
    this.root.replaceChildren();
  }

  /** Reads the table's columns again: after one was added, or its total changed. */
  readColumns() {
    this.cols = json(this.grid.columns());
    for (let col = this.order.length; col < this.cols.length; col++) this.order.push(col);
    this.numbers = this.cols.map((c) => {
      const decimals = c.agg === "count" && !numeric(c.kind) ? 0 : (this.formats[c.name]?.decimals ?? 2);
      return new Intl.NumberFormat("en-US", { minimumFractionDigits: decimals, maximumFractionDigits: decimals });
    });
    for (let pos = this.widths.length; pos <= this.cols.length; pos++) {
      const col = this.cols[pos - 1];
      this.widths[pos] = pos === 0 ? this.keyWidth : (this.formats[col.name]?.width ?? (numeric(col.kind) ? 112 : 124));
    }
    this.rebuildColumns();
  }

  isPinned(pos) {
    return pos === 0 || this.pinned.has(pos - 1);
  }

  trend(col) {
    return Boolean(this.formats[this.cols[col].name]?.trend);
  }

  /** Lays the columns out again after their order, pinning or visibility changed. */
  rebuildColumns() {
    this.cols = json(this.grid.columns());
    this.arrange();
    this.body.setAttribute("aria-colcount", this.display.length);

    this.headRow.replaceChildren();
    this.filterRow.replaceChildren();
    this.headCells = [];
    this.filterCells = [];
    this.filterInputs = [];
    for (const pos of this.display) {
      const col = this.cols[pos - 1];
      const cell = el("div", "wg-hcell" + (numeric(col?.kind) ? " wg-num" : ""));
      this.place(cell, pos);
      cell.setAttribute("role", "columnheader");
      cell.append(el("span", "wg-hname", pos === 0 ? this.keyTitle : col.name));
      if (col?.formula) {
        cell.append(el("span", "wg-fx", "ƒ"));
        cell.title = `${col.name} = ${col.formula}`;
      }
      if (pos > 0) cell.append(el("span", "wg-sort"));
      const more = el("button", "wg-more", "⋮");
      more.type = "button";
      more.setAttribute("aria-label", pos === 0 ? "Columns" : `${col.name} column menu`);
      more.addEventListener("click", (e) => {
        e.stopPropagation();
        this.openMenu(pos, more);
      });
      cell.append(more);
      if (pos > 0) {
        cell.addEventListener("pointerdown", (e) => this.dragHeader(e, pos, cell));
        cell.addEventListener("click", (e) => {
          if (this.dragged) return void (this.dragged = false);
          if (!e.target.closest(".wg-resize, .wg-more")) this.toggleSort(pos - 1, e.shiftKey);
        });
      }
      const handle = el("i", "wg-resize");
      handle.addEventListener("pointerdown", (e) => this.resize(e, pos));
      handle.addEventListener("dblclick", (e) => {
        e.stopPropagation();
        this.fit(pos);
      });
      handle.title = "Drag to resize; double-click to fit";
      cell.append(handle);
      this.headRow.append(cell);
      this.headCells[pos] = cell;

      const filter = el("div", "wg-fcell");
      this.place(filter, pos);
      if (pos > 0) filter.append(this.filterInput(pos - 1, col));
      this.filterRow.append(filter);
      this.filterCells[pos] = filter;
    }

    this.footRow.replaceChildren();
    this.footRow._cells = [];
    for (const pos of this.display) this.footRow.append((this.footRow._cells[pos] = this.makeCell(pos)));
    this.footRow._cells[0].textContent = "Total";
    for (const row of this.live.values()) row.remove();
    for (const row of this.spare) row.remove();
    this.live.clear();
    this.spare = [];
    this.before.clear();
    this.editing = null;
    this.showSort();
  }

  /** Works out which columns are on screen and in what order: the key, the pinned, the rest. */
  arrange() {
    const shown = this.order.filter((col) => !this.hidden.has(col));
    this.display = [
      0,
      ...shown.filter((c) => this.pinned.has(c)).map((c) => c + 1),
      ...shown.filter((c) => !this.pinned.has(c)).map((c) => c + 1),
    ];
    this.lastPinned = this.display.filter((pos) => this.isPinned(pos)).at(-1);
    this.trendCols = this.display.filter((pos) => pos > 0 && this.trend(pos - 1)).map((pos) => pos - 1);
    this.applyWidths();
    // A block is counted in places across the screen, which have just changed.
    if (this.range) {
      this.range = null;
      this.rangeChanged();
    }
  }

  /** Gives a cell its column's width, and pins it if its column is pinned. Returns the cell. */
  place(cell, pos) {
    const pinned = this.isPinned(pos);
    cell.style.width = `var(--wg-w${pos})`;
    cell._pos = pos;
    cell.classList.toggle("wg-pin", pinned);
    cell.classList.toggle("wg-pin-edge", pinned && pos === this.lastPinned);
    cell.style.left = pinned ? `var(--wg-l${pos})` : "";
    return cell;
  }

  filterInput(col, info) {
    const input = el("input");
    input.type = "text";
    input.spellcheck = false;
    input.value = info.filter;
    input.placeholder = numeric(info.kind) ? ">100, 5..10" : info.kind === "bool" ? "true / false" : "contains";
    input.setAttribute("aria-label", `Filter ${info.name}`);
    this.filterInputs[col] = input;
    let timer = 0;
    input.addEventListener("input", () => {
      clearTimeout(timer);
      timer = setTimeout(async () => {
        try {
          await this.table.filter(this.cols[col].name, input.value);
          input.classList.remove("wg-bad");
          input.title = "";
          this.body.scrollTop = 0;
          this.refresh();
        } catch (error) {
          input.classList.add("wg-bad");
          input.title = error.message;
        }
      }, 140);
    });
    return input;
  }

  makeCell(pos) {
    const col = this.cols[pos - 1];
    const cell = el("div", "wg-cell" + (numeric(col?.kind) ? " wg-num" : "") + (col?.formula ? " wg-calc" : ""));
    this.place(cell, pos);
    cell.setAttribute("role", pos === 0 ? "rowheader" : "gridcell");
    cell._text = "";
    if (pos > 0 && this.trend(pos - 1)) {
      cell.classList.add("wg-trend");
      const svg = document.createElementNS(SVG, "svg");
      svg.setAttribute("viewBox", "0 0 44 16");
      svg.setAttribute("class", "wg-spark");
      svg.setAttribute("aria-hidden", "true");
      cell._path = svg.appendChild(document.createElementNS(SVG, "path"));
      cell._spark = svg;
      cell._label = el("span");
      cell.append(svg, cell._label);
    }
    return cell;
  }

  applyWidths() {
    let total = 0;
    let pinnedWidth = 0;
    for (const pos of this.display) {
      const width = this.widths[pos];
      this.root.style.setProperty(`--wg-w${pos}`, `${width}px`);
      if (this.isPinned(pos)) {
        this.root.style.setProperty(`--wg-l${pos}`, `${pinnedWidth}px`);
        pinnedWidth += width;
      }
      total += width;
    }
    this.pinnedWidth = pinnedWidth;
    this.root.style.setProperty("--wg-width", `${total}px`);
  }

  /**
   * Sets a column's width to what its header, its total and the rows drawn now
   * need. Rows out of view are not measured: with a hundred thousand, that
   * would mean drawing them all.
   */
  fit(pos) {
    const range = document.createRange();
    const live = [...this.live.values()].map((row) => row._cells[pos]);
    let widest = 0;
    for (const cell of [this.headCells[pos], this.footRow._cells[pos], ...live]) {
      if (!cell?.isConnected) continue;
      // What the cell holds, from its first glyph to its last, whatever its
      // width now; the menu button and the resize handle sit over it, so
      // they don't count, and the header keeps room for the menu button.
      let [left, right] = [Infinity, -Infinity];
      for (const node of cell.childNodes) {
        if (node.nodeType === Node.ELEMENT_NODE && getComputedStyle(node).position === "absolute") continue;
        range.selectNode(node);
        const box = range.getBoundingClientRect();
        if (box.width === 0) continue;
        left = Math.min(left, box.left);
        right = Math.max(right, box.right);
      }
      if (right < left) continue;
      const style = getComputedStyle(cell);
      const padding = parseFloat(style.paddingLeft) + parseFloat(style.paddingRight);
      const menu = cell === this.headCells[pos] ? 28 : 0;
      widest = Math.max(widest, right - left + padding + menu);
    }
    if (widest === 0) return;
    this.widths[pos] = Math.min(640, Math.max(56, Math.ceil(widest) + 2));
    this.applyWidths();
  }

  /** Fits one column's width to what it shows (`col` is its position; -1 for the key column). */
  fitColumn(col) {
    this.fit(col + 1);
  }

  /** Fits every column on screen to what it shows. */
  fitColumns() {
    for (const pos of this.display) this.fit(pos);
  }

  resize(e, pos) {
    e.preventDefault();
    e.stopPropagation();
    const handle = e.currentTarget;
    const [startX, startWidth] = [e.clientX, this.widths[pos]];
    handle.setPointerCapture(e.pointerId);
    const move = (m) => {
      this.widths[pos] = Math.max(56, Math.round(startWidth + m.clientX - startX));
      this.applyWidths();
    };
    const done = () => {
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", done);
      handle.removeEventListener("pointercancel", done);
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", done);
    handle.addEventListener("pointercancel", done);
  }

  // ------------------------------------------- column order, pins and menu

  /**
   * Dragging a header sideways moves its column, there and then: the whole
   * column follows the pointer, stepping past each neighbour as the pointer
   * crosses its middle. Dragging onto the pinned columns pins it, and
   * dragging a pinned column off them unpins it. Near the grid's edges the
   * grid scrolls.
   */
  dragHeader(e, pos, cell) {
    if (e.button !== 0 || e.target.closest(".wg-resize, .wg-more")) return;
    const col = pos - 1;
    const startX = e.clientX;
    let x = startX;
    let dragging = false;
    let frame = 0;
    cell.setPointerCapture(e.pointerId);

    const settle = () => {
      const rootBox = this.root.getBoundingClientRect();
      const onPinned = x - rootBox.left < this.pinnedWidth;
      let moved = onPinned !== this.pinned.has(col);
      if (moved && onPinned) this.pinned.add(col);
      else if (moved) this.pinned.delete(col);
      // Its place among the columns of its kind: after each one whose middle the pointer has passed.
      const zone = this.display.filter((p) => p > 0 && this.pinned.has(p - 1) === this.pinned.has(col)).map((p) => p - 1);
      const others = zone.filter((c) => c !== col);
      const place = others.filter((c) => {
        const box = this.headCells[c + 1].getBoundingClientRect();
        return x > box.left + box.width / 2;
      }).length;
      moved ||= place !== zone.indexOf(col);
      if (!moved) return;
      this.order.splice(this.order.indexOf(col), 1);
      const at = others.length === 0 ? this.order.length : place < others.length ? this.order.indexOf(others[place]) : this.order.indexOf(others.at(-1)) + 1;
      this.order.splice(at, 0, col);
      this.reflow();
    };
    // While the pointer rests near an edge the grid keeps scrolling, and the column keeps up.
    const scroll = () => {
      const box = this.body.getBoundingClientRect();
      const [left, right] = [box.left + this.pinnedWidth, box.left + this.body.clientWidth];
      const before = this.body.scrollLeft;
      if (x > right - 48) this.body.scrollLeft += 16;
      else if (x >= left && x < left + 48) this.body.scrollLeft -= 16;
      if (this.body.scrollLeft !== before) settle();
      frame = requestAnimationFrame(scroll);
    };
    const move = (m) => {
      x = m.clientX;
      if (!dragging) {
        if (Math.abs(x - startX) < 5) return;
        dragging = true;
        this.closeMenu();
        this.root.classList.add("wg-moving-column");
        this.markColumn(pos, true);
        frame = requestAnimationFrame(scroll);
      }
      settle();
    };
    const done = () => {
      cell.removeEventListener("pointermove", move);
      cell.removeEventListener("pointerup", done);
      cell.removeEventListener("pointercancel", done);
      if (!dragging) return;
      cancelAnimationFrame(frame);
      this.root.classList.remove("wg-moving-column");
      this.markColumn(pos, false);
      // The click that follows a drag is not a request to sort.
      this.dragged = true;
      setTimeout(() => (this.dragged = false));
    };
    cell.addEventListener("pointermove", move);
    cell.addEventListener("pointerup", done);
    cell.addEventListener("pointercancel", done);
  }

  /** Every cell on screen of the column at `pos`: header, filter, total and rows. */
  columnCells(pos) {
    const rows = [...this.live.values(), ...this.spare];
    return [this.headCells[pos], this.filterCells[pos], this.footRow._cells[pos], ...rows.map((row) => row._cells[pos])];
  }

  markColumn(pos, on) {
    for (const cell of this.columnCells(pos)) cell.classList.toggle("wg-moving", on);
  }

  /**
   * Puts the columns on screen in their order again, moving the cells there
   * are rather than making new ones. For a change of order or pinning; a
   * column shown or hidden needs `layoutChanged`.
   */
  reflow() {
    this.arrange();
    const rows = [this.footRow, ...this.live.values(), ...this.spare];
    for (const pos of this.display) {
      this.headRow.append(this.place(this.headCells[pos], pos));
      this.filterRow.append(this.place(this.filterCells[pos], pos));
      for (const row of rows) row.append(this.place(row._cells[pos], pos));
    }
  }

  layoutChanged() {
    this.rebuildColumns();
    this.draw();
  }

  setPinned(col, pinned) {
    if (pinned) this.pinned.add(col);
    else this.pinned.delete(col);
    this.reflow();
  }

  setHidden(col, hidden) {
    if (hidden) this.hidden.add(col);
    else this.hidden.delete(col);
    this.layoutChanged();
  }

  /** Column order, pins, hidden columns and widths, by column name, to keep and give back. */
  getLayout() {
    const name = (col) => this.cols[col].name;
    return {
      order: this.order.map(name),
      hidden: [...this.hidden].map(name),
      pinned: [...this.pinned].map(name),
      widths: Object.fromEntries(this.widths.map((width, pos) => [pos === 0 ? "" : name(pos - 1), width])),
    };
  }

  setLayout(state) {
    const find = (name) => this.cols.findIndex((c) => c.name === name);
    const known = (names) => (names ?? []).map(find).filter((col) => col >= 0);
    const order = known(state.order);
    this.order = [...order, ...this.cols.map((_, col) => col).filter((col) => !order.includes(col))];
    this.hidden = new Set(known(state.hidden));
    this.pinned = new Set(known(state.pinned));
    for (const [name, width] of Object.entries(state.widths ?? {})) {
      const pos = name === "" ? 0 : find(name) + 1;
      if (name === "" || pos > 0) this.widths[pos] = width;
    }
    this.layoutChanged();
  }

  openMenu(pos, anchor) {
    const reopen = this.menuPos !== pos;
    this.closeMenu();
    if (!reopen) return;
    const col = pos - 1;
    const info = this.cols[col];
    const menu = el("div", "wg-menu");
    menu.setAttribute("role", "menu");
    const item = (label, action) => {
      const button = el("button", "wg-item", label);
      button.type = "button";
      button.setAttribute("role", "menuitem");
      button.addEventListener("click", () => {
        this.closeMenu();
        action();
      });
      menu.append(button);
    };
    if (pos > 0) {
      item("Sort ascending", () => this.setSort([{ col, desc: false }]));
      item("Sort descending", () => this.setSort([{ col, desc: true }]));
      if (this.sort.some((s) => s.col === col)) {
        item("Clear sort", () => this.setSort(this.sort.filter((s) => s.col !== col)));
      } else if (this.sort.length > 0) {
        // The same as Shift-clicking the header, for when there is no Shift key.
        item("Then sort ascending", () => this.setSort([...this.sort, { col, desc: false }]));
        item("Then sort descending", () => this.setSort([...this.sort, { col, desc: true }]));
      }
      menu.append(el("hr"));
      item(this.pinned.has(col) ? "Unpin" : "Pin to the left", () => this.setPinned(col, !this.pinned.has(col)));
      item("Hide column", () => this.setHidden(col, true));
      item("Fit width", () => this.fit(pos));
      if (info.groupable) {
        if (this.groupCol === col) item("Stop grouping", () => this.setGroup(-1));
        else item(`Group by ${info.name}`, () => this.setGroup(col));
      }
      menu.append(el("hr"));
      this.totalChoice(menu, col, info);
      if (info.kind === "text") this.valueFilter(menu, col);
    }
    menu.append(el("div", "wg-menu-title", "Columns"));
    this.cols.forEach((c, k) => {
      const label = el("label", "wg-check");
      const box = el("input");
      box.type = "checkbox";
      box.checked = !this.hidden.has(k);
      box.addEventListener("change", () => this.setHidden(k, !box.checked));
      label.append(box, el("span", "", c.name));
      menu.append(label);
    });
    menu.append(el("hr"));
    item("Fit all columns", () => this.fitColumns());
    item("Reset columns", () => {
      this.order = this.cols.map((_, k) => k);
      this.hidden.clear();
      this.pinned.clear();
      this.layoutChanged();
    });

    this.root.append(menu);
    const [box, rootBox] = [anchor.getBoundingClientRect(), this.root.getBoundingClientRect()];
    menu.style.left = `${Math.max(4, Math.min(box.left - rootBox.left, rootBox.width - menu.offsetWidth - 4))}px`;
    menu.style.top = `${box.bottom - rootBox.top + 4}px`;
    menu.style.maxHeight = `${Math.max(120, rootBox.height - (box.bottom - rootBox.top) - 12)}px`;
    this.menu = menu;
    this.menuPos = pos;
    document.addEventListener("pointerdown", this.outside, true);
    document.addEventListener("keydown", this.escape);
  }

  /** Adds the choice of a column's total to its menu. */
  totalChoice(menu, col, info) {
    menu.append(el("div", "wg-menu-title", "Total"));
    const chips = el("div", "wg-chips");
    for (const agg of numeric(info.kind) ? ["sum", "avg", "min", "max", "count", "none"] : ["count", "none"]) {
      const chip = el("button", "wg-chip", agg);
      chip.type = "button";
      chip.setAttribute("aria-pressed", String(info.agg === agg));
      chip.addEventListener("click", async () => {
        this.closeMenu();
        try {
          await this.grid.set_agg(col, agg);
        } catch (error) {
          return this.onError(error.message);
        }
        // In a worker, the worker's table has done this already.
        if (typeof this.table.connect !== "function") this.table.columnsChanged();
        this.refresh();
      });
      chips.append(chip);
    }
    menu.append(chips, el("hr"));
  }

  /** Adds a tick list of a text column's values to its menu; unticking one filters it out. */
  valueFilter(menu, col) {
    menu.append(el("div", "wg-menu-title", "Show values"));
    // The list is filled in when the engine answers (at once, unless it is in a worker).
    const holder = el("div");
    menu.append(holder, el("hr"));
    Promise.resolve(this.grid.values(col, 60)).then(
      (answer) => this.listValues(holder, col, json(answer)),
      (error) => holder.append(el("div", "wg-menu-note", error.message)),
    );
  }

  listValues(menu, col, { more, values }) {
    if (more) {
      menu.append(el("div", "wg-menu-note", "Too many to list. Type in the filter box instead."));
    } else {
      // `=a|b` in the filter box is the list of ticked values; anything else leaves all ticked.
      const typed = this.filterInputs[col].value.trim();
      const ticked = typed.startsWith("=") ? new Set(typed.slice(1).split("|").map((v) => v.trim().toLowerCase())) : null;
      const boxes = values.map(([value, rows]) => {
        const label = el("label", "wg-check");
        const box = el("input");
        box.type = "checkbox";
        box.checked = !ticked || ticked.has(value.toLowerCase());
        box.addEventListener("change", () => {
          const chosen = boxes.filter((b) => b.box.checked).map((b) => b.value);
          const all = chosen.length === boxes.length || chosen.length === 0;
          this.setFilter(col, all ? "" : `=${chosen.join("|")}`);
        });
        label.append(box, el("span", "", value || "(blank)"), el("span", "wg-count", rows.toLocaleString("en-US")));
        menu.append(label);
        return { box, value };
      });
    }
  }

  /** Sets a column's filter as if it had been typed in its box. */
  async setFilter(col, text) {
    const input = this.filterInputs[col];
    try {
      await this.table.filter(this.cols[col].name, text);
      input.value = text;
      input.classList.remove("wg-bad");
      this.body.scrollTop = 0;
      this.refresh();
    } catch (error) {
      this.onError(error.message);
    }
  }

  /**
   * Shows the grid as it was after an earlier update (see `grid.history_range()`),
   * or as it is now for `null`. Nothing can be edited while looking back.
   */
  setAsOf(epoch) {
    this.stopEdit(false, 0, true);
    try {
      Promise.resolve(this.grid.set_as_of(epoch ?? -1)).catch((error) => this.onError(error.message));
    } catch (error) {
      this.onError(error.message);
    }
    this.refresh();
  }

  closeMenu() {
    if (!this.menu) return;
    this.menu.remove();
    this.menu = null;
    this.menuPos = null;
    document.removeEventListener("pointerdown", this.outside, true);
    document.removeEventListener("keydown", this.escape);
  }

  // ------------------------------------------------- sorting and filtering

  toggleSort(col, add) {
    const at = this.sort.findIndex((s) => s.col === col);
    const next = at < 0 ? { col, desc: false } : this.sort[at].desc ? null : { col, desc: true };
    let sort = next ? [next] : [];
    if (add) {
      sort = [...this.sort];
      if (at < 0) sort.push(next);
      else if (next) sort[at] = next;
      else sort.splice(at, 1);
    }
    this.setSort(sort);
  }

  setSort(sort) {
    this.sort = sort;
    this.attempt(() => this.table.sort(sort.map((s) => ({ column: this.cols[s.col].name, desc: s.desc }))));
    this.showSort();
    this.refresh();
  }

  showSort() {
    for (const pos of this.display) {
      const mark = this.headCells[pos].querySelector(".wg-sort");
      if (!mark) continue;
      const at = this.sort.findIndex((s) => s.col === pos - 1);
      mark.textContent = at < 0 ? "" : (this.sort[at].desc ? "▼" : "▲") + (this.sort.length > 1 ? at + 1 : "");
      this.headCells[pos].setAttribute("aria-sort", at < 0 ? "none" : this.sort[at].desc ? "descending" : "ascending");
    }
  }

  setQuickFilter(text) {
    this.attempt(() => this.table.search(text));
    this.body.scrollTop = 0;
    this.refresh();
  }

  /** Groups by the column at this position, or by nothing if it is negative. */
  setGroup(col) {
    this.attempt(() => this.table.group(col < 0 ? null : this.cols[col].name));
    this.body.scrollTop = 0;
    // The grid follows the table's grouping when it is redrawn (see followView).
    this.refresh();
  }

  /** Runs a change to the table, telling the page if it fails, now (in the page) or later (in a worker). */
  attempt(change) {
    try {
      Promise.resolve(change()).catch((error) => this.onError(error.message));
    } catch (error) {
      this.onError(error.message);
    }
  }

  /**
   * Takes the sort, filters and grouping from the table, which may have been
   * set there rather than here: the header arrows, the filter boxes (but not
   * one being typed in) and `onGroup`.
   */
  followView() {
    const state = this.table.viewState();
    const col = (name) => this.cols.findIndex((c) => c.name === name);
    const sort = state.sort.map((s) => ({ col: col(s.column), desc: s.desc })).filter((s) => s.col >= 0);
    if (JSON.stringify(sort) !== JSON.stringify(this.sort)) {
      this.sort = sort;
      this.showSort();
    }
    this.filterInputs.forEach((input, c) => {
      const text = state.filters[this.cols[c]?.name] ?? "";
      if (input && input !== document.activeElement && input.value !== text) {
        input.value = text;
        input.classList.remove("wg-bad");
      }
    });
    const group = state.group === null ? -1 : col(state.group);
    if (group !== this.groupCol) {
      this.groupCol = group;
      this.onGroup(group);
    }
  }

  expandAll(open) {
    this.grid.expand_all(open);
    this.draw();
  }

  /** Opens a group and scrolls to it. */
  async revealGroup(key) {
    if (!(await this.grid.group_is_open(key))) await this.grid.toggle_group(key);
    const line = await this.grid.group_line(key);
    if (line < 0) return;
    this.draw();
    this.body.scrollTop = line * this.rowH;
    this.setFocus({ line, pos: 0, kind: "group", key, col: -1 });
    this.draw();
  }

  // ------------------------------------------------------ loading on scroll

  nearEnd() {
    return this.viewLen * this.rowH - (this.body.scrollTop + this.body.clientHeight) < 24 * this.rowH;
  }

  /**
   * Asks `loadMore` for more rows, unless it is already loading or has said
   * there are no more. Runs by itself as the person nears the end. If
   * `loadMore` throws, the grid says so and asks again next time.
   */
  async fetchMore() {
    if (!this.loadMore || this.loading || this.done) return;
    this.loading = true;
    this.onStatus({ loading: true, done: false });
    const before = this.viewLen;
    let more = false;
    let failed = null;
    try {
      more = await this.loadMore();
    } catch (error) {
      failed = error instanceof Error ? error.message : String(error);
    }
    if (this.destroyed) return;
    this.loading = false;
    if (failed !== null) {
      // Not done: the next scroll near the end, or fetchMore(), tries again.
      this.onError(failed);
      this.refresh();
      this.onStatus({ loading: false, done: false, failed: true, error: failed });
      return;
    }
    this.done = !more;
    this.refresh();
    this.onStatus({ loading: false, done: this.done, failed: false });
    // Still at the end, and the view grew: keep going. If it did not grow
    // (a filter hides the new rows, or groups are closed), wait to be asked.
    if (more && this.viewLen > before && this.nearEnd()) this.fetchMore();
  }

  // ---------------------------------------------------------------- drawing

  /** Recalculates the table now (rather than at the end of this task) and redraws. */
  refresh() {
    return this.table.flush();
  }

  /** Draws what a recalculation of the table changed. */
  redraw(report) {
    if (this.destroyed) return;
    if (report.columns) this.readColumns();
    if (report.view || report.columns) this.followView();
    this.epoch = report.epoch;
    this.draw();
    this.onStatus({
      ...report,
      ms: performance.now() - report.started,
      rows: this.grid.row_count(),
      filtered: this.grid.filtered_count(),
      groups: this.grid.group_count(),
      selected: this.selected.size,
    });
    if (this.focus) this.onFocus(this.focus);
    if (this.range && this.rangeCells <= 20000 && report.changed > 0) this.rangeChanged();
    return report;
  }

  /** Draws the rows in view again, asking `rowClass` for their classes: for when they have changed. */
  redrawRows() {
    if (!this.destroyed) this.draw();
  }

  schedule() {
    cancelAnimationFrame(this.frame);
    this.frame = requestAnimationFrame(() => this.draw());
  }

  format(col, value) {
    if (value === null) return "";
    if (typeof value === "number") return this.numbers[col].format(value);
    // A decimal comes as text, and is formatted as text: exactly, without
    // becoming a JavaScript number on the way.
    if (typeof value === "string" && this.cols[col].kind === "decimal") return this.numbers[col].format(value);
    if (typeof value === "object") return "#ERROR";
    return String(value);
  }

  /** Draws the lines in view. */
  draw() {
    const viewLen = this.grid.view_len();
    if (viewLen !== this.viewLen) {
      this.viewLen = viewLen;
      this.spacer.style.height = `${viewLen * this.rowH}px`;
      this.empty.hidden = viewLen > 0;
      this.body.setAttribute("aria-rowcount", viewLen + 2);
    }
    this.box = this.rangeBox();
    const first = Math.max(0, Math.floor(this.body.scrollTop / this.rowH) - OVERSCAN);
    const count = Math.ceil(this.body.clientHeight / this.rowH) + 2 * OVERSCAN;
    const lines = json(this.grid.rows(first, count));
    const last = first + lines.length;
    const trends = this.trendCols.map((col) => json(this.grid.trends(first, lines.length, col, TREND_POINTS)));

    for (const [line, row] of this.live) {
      if (line < first || line >= last) {
        if (this.editing?.row === row) this.stopEdit(false, 0, true);
        row.hidden = true;
        this.live.delete(line);
        this.spare.push(row);
      }
    }
    const seen = new Map();
    lines.forEach((data, k) => {
      const line = first + k;
      let row = this.live.get(line);
      if (!row) {
        row = this.spare.pop() ?? this.makeRow();
        row.hidden = false;
        row.style.transform = `translateY(${line * this.rowH}px)`;
        this.live.set(line, row);
      }
      this.drawRow(row, line, data, seen);
      this.trendCols.forEach((col, t) => this.spark(row._cells[col + 1], trends[t][k]));
    });

    const totals = json(this.grid.totals());
    const totalsBefore = this.before.get("total");
    const cells = this.footRow._cells;
    for (const pos of this.display) {
      if (pos > 0) this.setCell(cells[pos], pos - 1, totals[pos - 1], totalsBefore?.[pos - 1]);
    }
    for (const col of this.trendCols) this.spark(cells[col + 1], json(this.grid.total_trend(col, TREND_POINTS)));
    seen.set("total", totals);
    this.before = seen;
    cells[0].textContent = this.grid.filtered_count() < this.grid.row_count() ? "Total (filtered)" : "Total";
  }

  makeRow() {
    const row = el("div", "wg-row");
    row.setAttribute("role", "row");
    row._cells = [];
    for (const pos of this.display) row.append((row._cells[pos] = this.makeCell(pos)));
    this.rows.append(row);
    return row;
  }

  drawRow(row, line, data, seen) {
    const group = data.g !== undefined;
    const id = group ? `g:${data.g}` : `r:${data.k}`;
    if (this.editing?.row === row && this.editing.key !== data.k) this.stopEdit(false, 0, true);
    row._line = line;
    row._data = data;
    // The app's classes for the row, by its key: asked for each time it is drawn,
    // so they follow the row as it scrolls and moves.
    const own = !group && this.rowClass ? this.rowClass(data.k) : "";
    row.className =
      "wg-row" +
      (group ? " wg-group" : "") +
      (line % 2 ? " wg-odd" : "") +
      (!group && this.selected.has(data.k) ? " wg-selected" : "") +
      (own ? ` ${own}` : "");
    const cells = row._cells;
    const first = cells[0];
    const label = group ? `${data.g || "(blank)"} ${data.n}` : data.k;
    if (first._text !== label) {
      first._text = label;
      if (group) first.replaceChildren(el("span", "wg-gname", data.g || "(blank)"), el("span", "wg-gcount", data.n.toLocaleString("en-US")));
      else first.textContent = label;
    }
    const was = this.before.get(id);
    const focus = this.focus && this.focus.line === line ? this.focus.pos : -1;
    const box = this.box && !group && line >= this.box.l0 && line <= this.box.l1 ? this.box : null;
    row.setAttribute("aria-rowindex", line + 2);
    if (group) row.setAttribute("aria-expanded", String(data.o));
    else row.removeAttribute("aria-expanded");
    this.display.forEach((pos, across) => {
      const cell = cells[pos];
      const picked = box !== null && across >= box.a0 && across <= box.a1;
      cell.classList.toggle("wg-focus", pos === focus);
      cell.classList.toggle("wg-range", picked);
      cell.setAttribute("aria-selected", String(picked || (pos === 0 && !group && this.selected.has(data.k))));
      if (pos === 0 || cell === this.editing?.cell) return;
      this.setCell(cell, pos - 1, data.c[pos - 1], was?.[pos - 1]);
    });
    seen.set(id, data.c);
  }

  /** Shows a value in a cell, and flashes it if it is a number that moved. */
  setCell(cell, col, value, was) {
    const text = this.format(col, value);
    if (cell._text !== text) {
      (cell._label ?? cell).textContent = cell._text = text;
      cell.title = value !== null && typeof value === "object" ? value.e : "";
    }
    // A decimal's sign and which way it moved, from its nearest number.
    const decimal = this.cols[col].kind === "decimal";
    const number = (v) => (typeof v === "number" ? v : decimal && typeof v === "string" ? Number(v) : null);
    const now = number(value);
    const before = number(was);
    if (this.formats[this.cols[col].name]?.signed) {
      cell.classList.toggle("wg-neg", now !== null && now < 0);
      cell.classList.toggle("wg-pos", now !== null && now > 0);
    }
    if (now !== null && before !== null && value !== was) {
      cell.animate(
        [{ backgroundColor: `var(${now > before ? "--wg-up" : "--wg-down"})` }, { backgroundColor: "transparent" }],
        { duration: 900, easing: "cubic-bezier(0.2, 0.6, 0.2, 1)" },
      );
    }
  }

  /** Draws how a cell's number moved over recent updates, as a small line. */
  spark(cell, numbers) {
    const key = numbers ? numbers.join() : "";
    if (cell._sparkKey === key) return;
    cell._sparkKey = key;
    if (!numbers) return void cell._path.setAttribute("d", "");
    const [low, high] = [Math.min(...numbers), Math.max(...numbers)];
    const step = 44 / (numbers.length - 1);
    const y = (v) => (high === low ? 8 : 14.5 - ((v - low) / (high - low)) * 13).toFixed(1);
    cell._path.setAttribute("d", numbers.map((v, k) => `${k ? "L" : "M"}${(k * step).toFixed(1)} ${y(v)}`).join(""));
    const [from, to] = [numbers[0], numbers.at(-1)];
    cell._spark.setAttribute("class", "wg-spark" + (to > from ? " wg-rose" : to < from ? " wg-fell" : ""));
  }

  // ------------------------------------------------- pointer and keyboard

  hit(e) {
    const cell = e.target.closest(".wg-cell");
    const row = cell?.parentElement;
    if (!row || row._data === undefined) return null;
    return { cell, row, line: row._line, pos: cell._pos, data: row._data };
  }

  clicked(e) {
    const hit = this.hit(e);
    if (!hit || e.target.closest("input")) return;
    if (hit.data.g !== undefined) {
      if (hit.pos === 0) this.grid.toggle_group(hit.data.g);
      this.focusLine(hit.line, hit.pos, hit.data);
      this.draw();
      return;
    }
    if (this.ranged) return; // the click that ends a drag over cells
    if (hit.pos > 0) {
      // A cell: Shift-click picks out the block from the focused cell to it.
      if (e.shiftKey && this.focus?.kind === "cell" && this.focus.pos > 0) {
        this.setRange(this.focus.line, this.display.indexOf(this.focus.pos), hit.line, this.display.indexOf(hit.pos));
      } else {
        this.range = null;
        this.focusLine(hit.line, hit.pos, hit.data);
        this.rangeChanged();
      }
      this.draw();
      return;
    }
    // The key column selects rows.
    const key = hit.data.k;
    if (e.shiftKey && this.anchor !== null) {
      const [low, high] = [Math.min(this.anchor, hit.line), Math.max(this.anchor, hit.line)];
      if (!(e.metaKey || e.ctrlKey)) this.selected.clear();
      // The rows between may be out of view; a worker sends them when asked.
      this.lines(low, Math.min(high - low + 1, 20000)).then((between) => {
        for (const line of between) if (line.k !== undefined) this.selected.add(line.k);
        this.onStatus({ selected: this.selected.size });
        this.draw();
      });
    } else if (e.metaKey || e.ctrlKey) {
      if (!this.selected.delete(key)) this.selected.add(key);
      this.anchor = hit.line;
    } else if (this.selected.size === 1 && this.selected.has(key)) {
      this.selected.clear(); // clicking the one selected row lets it go
    } else {
      this.selected.clear();
      this.selected.add(key);
      this.anchor = hit.line;
    }
    this.range = null;
    this.focusLine(hit.line, hit.pos, hit.data);
    this.rangeChanged();
    this.draw();
    this.onStatus({ selected: this.selected.size });
  }

  // ------------------------------------------------------- a block of cells

  /** The block picked out, as first and last line and first and last place across; or null. */
  rangeBox() {
    const r = this.range;
    if (!r) return null;
    return {
      l0: Math.min(r.line, r.toLine),
      l1: Math.max(r.line, r.toLine),
      a0: Math.min(r.across, r.toAcross),
      a1: Math.max(r.across, r.toAcross),
    };
  }

  /** The block's columns, as the engine numbers them, left to right. */
  rangeCols(box) {
    return Uint32Array.from(this.display.slice(box.a0, box.a1 + 1).filter((pos) => pos > 0).map((pos) => pos - 1));
  }

  /** Picks out the block from one cell (which gets the focus) to another. */
  setRange(line, across, toLine, toAcross) {
    const last = this.display.length - 1;
    const clamp = (a) => Math.min(Math.max(a, 1), last);
    toLine = Math.min(Math.max(toLine, 0), this.viewLen - 1);
    [across, toAcross] = [clamp(across), clamp(toAcross)];
    this.range = line === toLine && across === toAcross ? null : { line, across, toLine, toAcross };
    if (this.focus?.line !== line || this.focus?.pos !== this.display[across]) {
      const [data] = json(this.grid.rows(line, 1));
      if (data) this.focusLine(line, this.display[across], data);
    }
    this.rangeChanged();
  }

  /** Tells the page what the block comes to (or that there is none). */
  rangeChanged() {
    const box = this.rangeBox();
    if (!box) return this.onStatus({ range: null });
    const cols = this.rangeCols(box);
    this.rangeCells = (box.l1 - box.l0 + 1) * cols.length;
    Promise.resolve(this.grid.range_stats(box.l0, box.l1 - box.l0 + 1, cols)).then(
      (stats) => this.onStatus({ range: json(stats) }),
      (error) => this.onError(error.message),
    );
  }

  /** The line and the place across of the cell under a point on the screen. */
  cellAt(x, y) {
    const box = this.body.getBoundingClientRect();
    const line = Math.floor((y - box.top + this.body.scrollTop) / this.rowH);
    // Pinned columns stay put; the rest are where the scrolling has left them.
    let at = x - box.left;
    if (at >= this.pinnedWidth) at += this.body.scrollLeft;
    let across = 0;
    for (let edge = 0; across < this.display.length - 1; across++) {
      edge += this.widths[this.display[across]];
      if (at < edge) break;
    }
    return { line: Math.min(Math.max(line, 0), this.viewLen - 1), across: Math.max(across, 1) };
  }

  /** Dragging over cells picks out a block, scrolling when the pointer nears an edge. */
  dragRange(e) {
    const hit = this.hit(e);
    if (!hit || hit.pos === 0 || hit.data.k === undefined || e.button !== 0 || e.shiftKey || e.target.closest("input")) return;
    const from = { line: hit.line, across: this.display.indexOf(hit.pos) };
    let [x, y] = [e.clientX, e.clientY];
    let dragging = false;
    let frame = 0;
    const settle = () => {
      const to = this.cellAt(x, y);
      if (!dragging && to.line === from.line && to.across === from.across) return;
      if (!dragging) {
        dragging = true;
        this.stopEdit(true, 0, true);
        this.root.classList.add("wg-ranging");
      }
      const r = this.range;
      if (r && r.toLine === to.line && r.toAcross === to.across) return;
      this.setRange(from.line, from.across, to.line, to.across);
      this.draw();
    };
    let over = false;
    const scroll = () => {
      if (over) return;
      const box = this.body.getBoundingClientRect();
      const before = [this.body.scrollTop, this.body.scrollLeft];
      if (dragging) {
        if (y > box.top + this.body.clientHeight - 24) this.body.scrollTop += this.rowH;
        else if (y < box.top + 24) this.body.scrollTop -= this.rowH;
        if (x > box.left + this.body.clientWidth - 24) this.body.scrollLeft += 24;
        else if (x < box.left + this.pinnedWidth + 24 && x >= box.left + this.pinnedWidth) this.body.scrollLeft -= 24;
      }
      if (before[0] !== this.body.scrollTop || before[1] !== this.body.scrollLeft) settle();
      frame = requestAnimationFrame(scroll);
    };
    const move = (m) => {
      [x, y] = [m.clientX, m.clientY];
      settle();
    };
    const done = () => {
      removeEventListener("pointermove", move);
      removeEventListener("pointerup", done);
      removeEventListener("pointercancel", done);
      over = true;
      cancelAnimationFrame(frame);
      if (!dragging) return;
      this.root.classList.remove("wg-ranging");
      this.ranged = true;
      setTimeout(() => (this.ranged = false));
      this.body.focus({ preventScroll: true });
    };
    addEventListener("pointermove", move);
    addEventListener("pointerup", done);
    addEventListener("pointercancel", done);
    frame = requestAnimationFrame(scroll);
  }

  focusLine(line, pos, data) {
    const group = data.g !== undefined;
    this.setFocus({ line, pos, kind: group ? "group" : "cell", key: group ? data.g : data.k, col: pos - 1 });
  }

  setFocus(focus) {
    this.focus = focus;
    for (const pos of this.display) {
      this.footRow._cells[pos].classList.toggle("wg-focus", focus.kind === "total" && pos === focus.pos);
    }
    this.say(focus);
    this.onFocus(focus);
  }

  /** Says where the focus is and what is there, for a screen reader. */
  say(focus) {
    const column = focus.pos === 0 ? this.keyTitle : this.cols[focus.col].name;
    if (focus.kind === "total") {
      this.said.textContent = `Total of ${column}: ${this.footRow._cells[focus.pos]._text || "none"}`;
      return;
    }
    const row = this.live.get(focus.line);
    const text = row?._cells[focus.pos]?._text;
    if (focus.kind === "group") {
      const data = row?._data;
      const state = data ? `, ${data.n} rows, ${data.o ? "open" : "closed"}` : "";
      this.said.textContent = focus.pos === 0 ? `Group ${focus.key || "blank"}${state}` : `${column} for group ${focus.key || "blank"}: ${text || "none"}`;
    } else {
      this.said.textContent = focus.pos === 0 ? `Row ${focus.key}` : `${column}, ${focus.key}: ${text ?? ""}`;
    }
  }

  /** Why the focused cell has its value, from the engine. */
  explainFocus() {
    const f = this.focus;
    if (!f || f.col < 0) return null;
    const name = this.cols[f.col].name;
    const title = f.kind === "total" ? `Total of ${name}` : f.kind === "group" ? `${name} for ${f.key || "(blank)"}` : `${name} of ${f.key}`;
    // What was asked for last, and when: an answer from a worker comes
    // later, so it is kept, and the page told again when it comes.
    const asking = `${f.kind}|${f.key}|${f.col}|${this.epoch}`;
    if (this.why?.asking === asking) return this.why.answer;
    let text;
    try {
      text = f.kind === "total" ? this.grid.explain_total(f.col) : f.kind === "group" ? this.grid.explain_group(f.key, f.col) : this.grid.explain(f.key, f.col);
    } catch (error) {
      return { title: name, text: error.message };
    }
    if (typeof text?.then !== "function") return { title, text };
    const before = this.why?.answer.title === title ? this.why.answer : { title, text: "Working it out…" };
    this.why = { asking, answer: before };
    text.then(
      (answer) => {
        if (this.why?.asking !== asking || this.destroyed) return;
        this.why.answer = { title, text: answer };
        if (this.focus) this.onFocus(this.focus);
      },
      (error) => {
        if (this.why?.asking === asking) this.why.answer = { title: name, text: error.message };
      },
    );
    return before;
  }

  /** Moves the focus to a line and to the column `across` places along the screen. */
  moveFocus(line, across) {
    if (this.viewLen <= 0) return;
    if (this.range) {
      this.range = null;
      this.rangeChanged();
    }
    line = Math.min(Math.max(line, 0), this.viewLen - 1);
    const pos = this.display[Math.min(Math.max(across, 0), this.display.length - 1)];
    this.reveal(line, pos);
    const [data] = json(this.grid.rows(line, 1));
    if (!data) return;
    this.focusLine(line, pos, data);
    this.draw();
  }

  /** Scrolls so that a cell is in view, clear of the pinned columns. */
  reveal(line, pos) {
    const top = line * this.rowH;
    if (top < this.body.scrollTop) this.body.scrollTop = top;
    else if (top + this.rowH > this.body.scrollTop + this.body.clientHeight) this.body.scrollTop = top + this.rowH - this.body.clientHeight;
    if (!this.isPinned(pos)) {
      // Bring it into view, clear of the pinned columns.
      let left = 0;
      for (const p of this.display) {
        if (p === pos) break;
        left += this.widths[p];
      }
      if (left - this.pinnedWidth < this.body.scrollLeft) this.body.scrollLeft = left - this.pinnedWidth;
      else if (left + this.widths[pos] > this.body.scrollLeft + this.body.clientWidth) this.body.scrollLeft = left + this.widths[pos] - this.body.clientWidth;
    }
  }

  key(e) {
    if (this.editing || e.target !== this.body) return;
    const f = this.focus && this.focus.line >= 0 ? this.focus : { line: 0, pos: this.display[1] ?? 0 };
    const across = Math.max(0, this.display.indexOf(f.pos));
    const page = Math.max(1, Math.floor(this.body.clientHeight / this.rowH) - 1);
    const moves = {
      ArrowDown: [f.line + 1, across],
      ArrowUp: [f.line - 1, across],
      ArrowLeft: [f.line, across - 1],
      ArrowRight: [f.line, across + 1],
      PageDown: [f.line + page, across],
      PageUp: [f.line - page, across],
      Home: e.metaKey || e.ctrlKey ? [0, across] : [f.line, 0],
      End: e.metaKey || e.ctrlKey ? [this.viewLen - 1, across] : [f.line, this.display.length - 1],
    };
    const command = e.metaKey || e.ctrlKey;
    if (command && (e.key === "z" || e.key === "Z" || e.key === "y")) {
      e.preventDefault();
      this.undo(e.key === "y" || e.shiftKey);
    } else if (command && e.key === "a") {
      // Every cell.
      e.preventDefault();
      this.setRange(0, 1, this.viewLen - 1, this.display.length - 1);
      this.draw();
    } else if (e.key === "Escape" && this.range) {
      this.range = null;
      this.rangeChanged();
      this.draw();
    } else if (moves[e.key] && e.shiftKey && this.focus?.kind === "cell") {
      // Shift moves the far corner of the block, leaving the focus where it is.
      e.preventDefault();
      const far = this.range ?? { toLine: f.line, toAcross: across };
      const [dLine, dAcross] = [moves[e.key][0] - f.line, moves[e.key][1] - across];
      const to = [Math.min(Math.max(far.toLine + dLine, 0), this.viewLen - 1), Math.min(Math.max(far.toAcross + dAcross, 1), this.display.length - 1)];
      this.setRange(f.line, Math.max(across, 1), to[0], to[1]);
      this.reveal(to[0], this.display[to[1]]);
      this.draw();
    } else if (moves[e.key]) {
      e.preventDefault();
      this.moveFocus(...moves[e.key]);
    } else if (this.focus?.kind === "group" && (e.key === "Enter" || e.key === " ")) {
      e.preventDefault();
      this.grid.toggle_group(this.focus.key);
      this.draw();
    } else if (this.focus?.kind === "cell" && (e.key === "Enter" || e.key === "F2")) {
      e.preventDefault();
      this.edit(this.focus.line, this.focus.pos);
    } else if (this.focus?.kind === "cell" && this.focus.pos > 0 && e.key.length === 1 && !e.metaKey && !e.ctrlKey && !e.altKey) {
      // Typing over a cell starts editing it with what was typed.
      this.edit(this.focus.line, this.focus.pos, "");
    }
  }

  // ---------------------------------------------------------------- editing

  /** Opens an editor on a cell. Returns whether it could (only fields can be edited). */
  edit(line, pos, initial) {
    const row = this.live.get(line);
    const col = this.cols[pos - 1];
    if (!row || !col || row._data.k === undefined) return false;
    if (!col.editable) {
      this.onError(`${col.name} is calculated (${col.formula}), so it can't be edited.`);
      return false;
    }
    this.stopEdit(false);
    const cell = row._cells[pos];
    const input = el("input", "wg-editor");
    input.type = "text";
    input.spellcheck = false;
    const value = row._data.c[pos - 1];
    input.value = initial ?? (value === null || typeof value === "object" ? "" : String(value));
    this.editing = { row, cell, input, key: row._data.k, col: pos - 1 };
    cell.classList.add("wg-editing");
    cell.append(input);
    input.focus();
    if (initial === undefined) input.select();
    input.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Enter" && (e.metaKey || e.ctrlKey) && this.range) {
        // Fill the whole block with what was typed.
        const text = input.value;
        this.stopEdit(false, 0, true);
        this.fillRange(text);
      } else if (e.key === "Enter") this.stopEdit(true, 1);
      else if (e.key === "Escape") this.stopEdit(false);
      else if (e.key === "Tab") {
        e.preventDefault();
        this.stopEdit(true);
      }
    });
    input.addEventListener("blur", () => {
      if (this.editing?.input === input) this.stopEdit(true);
    });
    return true;
  }

  /** Closes the editor. `quiet` is for use while drawing: it leaves the redraw to the caller. */
  stopEdit(commit, moveDown = 0, quiet = false) {
    const editing = this.editing;
    if (!editing) return;
    this.editing = null;
    const text = editing.input.value;
    editing.input.remove();
    editing.cell.classList.remove("wg-editing");
    editing.cell._text = null; // draw it again
    if (commit) {
      // With a Table this runs at once; from a worker, an error comes later.
      this.personEdit("edit", [["set_cell", editing.key, editing.col, text]]).catch((error) => this.onError(error.message));
    }
    if (quiet) return;
    this.body.focus({ preventScroll: true });
    this.refresh();
    if (commit && moveDown && this.focus?.kind === "cell") {
      this.moveFocus(this.focus.line + moveDown, this.display.indexOf(this.focus.pos));
    }
  }

  // ------------------------------------------------- undo, copy and paste

  /** Puts back the last edit (or pasted block), or with `again` does it again. */
  async undo(again = false) {
    try {
      const [what] = await this.personEdit(again ? "redo" : "undo", [[again ? "redo" : "undo"]]);
      this.refresh();
      this.onNote(what ? `${again ? "Did again" : "Undid"}: ${what}.` : `Nothing to ${again ? "do again" : "undo"}.`);
    } catch (error) {
      this.onError(error.message);
    }
  }

  /** Copies the selected rows, or with one or none selected the focused cell, as tab-separated text. */
  copy(e) {
    if (this.editing) return;
    let text = null;
    const box = this.rangeBox();
    if (box) {
      text = this.grid.range_tsv(box.l0, box.l1 - box.l0 + 1, this.rangeCols(box));
      this.onNote(`Copied ${this.rangeCells.toLocaleString("en-US")} cells.`);
    } else if (this.selected.size > 1) {
      text = this.grid.tsv([...this.selected], Uint32Array.from(this.display.filter((pos) => pos > 0).map((pos) => pos - 1)));
      this.onNote(`Copied ${this.selected.size.toLocaleString("en-US")} rows.`);
    }
    if (typeof text?.then === "function") {
      // From a worker the text comes later, after the copy event, so it goes
      // to the clipboard through the browser's asynchronous clipboard.
      e.preventDefault();
      text
        .then((copied) => navigator.clipboard.writeText(copied))
        .catch((error) => this.onError(`Could not copy: ${error.message}`));
      return;
    }
    if (text === null && this.focus?.kind === "cell") {
      const data = this.live.get(this.focus.line)?._data;
      const value = this.focus.pos === 0 ? data?.k : data?.c[this.focus.col];
      if (value !== undefined && value !== null && typeof value !== "object") text = String(value);
    }
    if (text === null) return;
    e.preventDefault();
    e.clipboardData.setData("text/plain", text);
  }

  /**
   * Pastes tab-separated text into cells, from the focused cell rightwards
   * and downwards, as a spreadsheet does. Calculated columns and what does
   * not read as the column's kind are skipped. The whole paste is one step to undo.
   */
  paste(e) {
    if (this.editing) return;
    e.preventDefault();
    if (this.focus?.kind !== "cell") return this.onError("Click a cell to paste from there.");
    const block = e.clipboardData.getData("text/plain").replace(/\r/g, "").replace(/\n$/, "").split("\n").map((line) => line.split("\t"));
    const box = this.rangeBox();
    // One thing pasted onto a block fills the block.
    if (box && block.length === 1 && block[0].length === 1) return this.fillRange(block[0][0]);
    // Otherwise the paste goes from the block's first cell, or the focused one, rightwards and downwards.
    const across = box ? box.a0 : Math.max(1, this.display.indexOf(this.focus.pos));
    const top = box ? box.l0 : this.focus.line;
    // The rows from there down, passing over group lines.
    this.lines(top, block.length * 2 + 64).then(async (lines) => {
      const rows = lines.filter((line) => line.k !== undefined);
      const cells = [];
      let skipped = 0;
      block.slice(0, rows.length).forEach((texts, r) => {
        texts.forEach((text, c) => {
          const pos = this.display[across + c];
          if (pos === undefined || !this.cols[pos - 1].editable) skipped += 1;
          else cells.push([rows[r].k, pos - 1, text]);
        });
      });
      const done = await this.setCells(cells);
      this.refresh();
      const count = (n) => `${n.toLocaleString("en-US")} cell${n === 1 ? "" : "s"}`;
      skipped += done.skipped;
      this.onNote(`Pasted ${count(done.set)}${skipped ? `; skipped ${count(skipped)}` : ""}.`);
    });
  }

  /** Sets cells (`[key, col, text]`) as one step to undo, skipping those that will not take their text. */
  async setCells(cells) {
    if (!cells.length) return { set: 0, skipped: 0 };
    const results = await this.personEdit(
      "paste",
      cells.map(([key, col, text]) => ["set_cell", key, col, text]),
      { batch: true, tolerant: true },
    );
    const skipped = results.filter((r) => r !== null && typeof r === "object" && "error" in r).length;
    return { set: cells.length - skipped, skipped };
  }

  /**
   * Runs engine calls a person made (`[method, ...args]` each) with the cells
   * they change noted, tells `onEdit` what changed, and gives back each
   * call's result. With `batch` they are one step to undo; with `tolerant` a
   * call that fails gives `{ error }` and the rest still run. In a worker
   * they go as one message, so nothing else is set between them, and what
   * `onEdit` hears is only what the person did.
   */
  async personEdit(cause, calls, { batch = false, tolerant = false } = {}) {
    const all = [["record_edits"], ...calls, ["take_edits"]];
    let results;
    if (typeof this.grid.sequence === "function") {
      try {
        results = await this.grid.sequence(all, batch, tolerant);
      } catch (error) {
        // A call failed before the noting was taken: stop it.
        void this.grid.take_edits();
        throw error;
      }
    } else {
      results = [];
      if (batch) this.grid.begin_edits();
      try {
        for (const [method, ...args] of all) {
          try {
            results.push(this.grid[method](...args));
          } catch (error) {
            if (!tolerant) throw error;
            results.push({ error: error.message });
          }
        }
      } finally {
        if (batch) this.grid.end_edits();
        // Stop noting, even if a call failed.
        if (results.length < all.length) this.grid.take_edits();
      }
    }
    const noted = json(results.pop());
    results.shift();
    if (this.onEdit && noted.length) {
      const cells = noted.map((e) => ({ key: e.k, column: this.cols[e.c].name, before: cellOf(e.b), after: cellOf(e.a) }));
      this.onEdit({ cause, cells, removed: [] });
    }
    return results;
  }

  /** Lines of the view, which in a worker may be out of the rows it has sent. */
  async lines(first, count) {
    return typeof this.grid.fetchRows === "function" ? this.grid.fetchRows(first, count) : json(this.grid.rows(first, count));
  }

  /** Sets every cell of the block to one text, as one step to undo. */
  async fillRange(text) {
    const box = this.rangeBox();
    if (!box) return;
    const [filled] = await this.personEdit("fill", [["fill", box.l0, box.l1 - box.l0 + 1, this.rangeCols(box), text]]);
    const { set, skipped } = json(filled);
    this.body.focus({ preventScroll: true });
    this.refresh();
    const count = (n) => `${n.toLocaleString("en-US")} cell${n === 1 ? "" : "s"}`;
    this.onNote(`Filled ${count(set)}${skipped ? `; skipped ${count(skipped)}` : ""}.`);
  }

  // -------------------------------------------------------------- selection

  selectedKeys() {
    return [...this.selected];
  }

  /** Removes the selected rows from the grid. */
  removeSelected() {
    const keys = [...this.selected];
    // In a worker, as one message and one recalculation.
    if (typeof this.grid.batch === "function") this.grid.batch(() => keys.forEach((key) => this.grid.remove(key)));
    else for (const key of keys) this.grid.remove(key);
    if (this.focus?.kind === "cell" && this.selected.has(this.focus.key)) {
      this.focus = null;
      this.onFocus(null);
    }
    this.selected.clear();
    this.anchor = null;
    this.refresh();
    if (this.onEdit && keys.length) this.onEdit({ cause: "delete", cells: [], removed: keys });
  }
}
