// Draws a watershed grid. The rows, calculated columns, totals, filtering,
// sorting and grouping all live in WebAssembly (crates/watershed-grid); this
// file only draws the lines in view and passes on what the person does.
//
//   const view = new WatershedGrid(element, grid, { keyTitle: "trade" });
//   grid.set_cell("T-000001", 4, "500");
//   view.refresh();          // apply changes and redraw
//
// A column is known by its position in the engine (0, 1, 2…). On screen the
// key column comes first, then pinned columns, then the rest, in whatever
// order the person has dragged them into. A cell's `pos` is 0 for the key
// column and the column's position plus one otherwise.

const OVERSCAN = 6;
const TREND_POINTS = 32;
const SVG = "http://www.w3.org/2000/svg";

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

export class WatershedGrid {
  /**
   * @param {HTMLElement} root an element with a height
   * @param grid the WebAssembly `Grid`
   * @param options
   *   rowHeight, keyTitle,
   *   formats: { column: { decimals, signed, width, trend } }
   *     (`trend` draws how the cell moved over recent updates; needs `grid.keep_history`),
   *   loadMore: async () => boolean, called as the person nears the end: add
   *     rows to the grid and resolve to whether there may be more,
   *   onStatus(status), onFocus(focus), onError(message), onGroup(col)
   */
  constructor(root, grid, options = {}) {
    this.root = root;
    this.grid = grid;
    this.rowH = options.rowHeight ?? 30;
    this.keyTitle = options.keyTitle ?? "key";
    this.formats = options.formats ?? {};
    this.loadMore = options.loadMore ?? null;
    this.onStatus = options.onStatus ?? (() => {});
    this.onFocus = options.onFocus ?? (() => {});
    this.onGroup = options.onGroup ?? (() => {});
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
    this.columnsChanged();
  }

  // ------------------------------------------------------------- structure

  build() {
    this.root.classList.add("wg");
    this.root.style.setProperty("--wg-row", `${this.rowH}px`);
    this.root.replaceChildren();
    this.head = el("div", "wg-head");
    this.headRow = el("div", "wg-hrow");
    this.filterRow = el("div", "wg-frow");
    this.marker = el("div", "wg-marker");
    this.marker.hidden = true;
    this.head.append(this.headRow, this.filterRow, this.marker);
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
      this.schedule();
      if (this.nearEnd()) this.fetchMore();
    });
    this.rows.addEventListener("click", (e) => this.clicked(e));
    this.rows.addEventListener("dblclick", (e) => {
      const hit = this.hit(e);
      if (hit && hit.data.k !== undefined) this.edit(hit.line, hit.pos);
    });
    this.body.addEventListener("keydown", (e) => this.key(e));
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

  destroy() {
    this.destroyed = true;
    this.closeMenu();
    this.observer.disconnect();
    cancelAnimationFrame(this.frame);
    this.root.replaceChildren();
  }

  /** Call after adding a column: reads the columns again and redraws everything. */
  columnsChanged() {
    this.cols = JSON.parse(this.grid.columns());
    for (let col = this.order.length; col < this.cols.length; col++) this.order.push(col);
    this.numbers = this.cols.map((c) => {
      const decimals = c.agg === "count" && c.kind !== "number" ? 0 : (this.formats[c.name]?.decimals ?? 2);
      return new Intl.NumberFormat("en-US", { minimumFractionDigits: decimals, maximumFractionDigits: decimals });
    });
    for (let pos = this.widths.length; pos <= this.cols.length; pos++) {
      const col = this.cols[pos - 1];
      this.widths[pos] = pos === 0 ? 156 : (this.formats[col.name]?.width ?? (col.kind === "number" ? 112 : 124));
    }
    this.rebuildColumns();
    this.refresh();
  }

  isPinned(pos) {
    return pos === 0 || this.pinned.has(pos - 1);
  }

  trend(col) {
    return Boolean(this.formats[this.cols[col].name]?.trend);
  }

  /** Lays the columns out again after their order, pinning or visibility changed. */
  rebuildColumns() {
    this.cols = JSON.parse(this.grid.columns());
    const shown = this.order.filter((col) => !this.hidden.has(col));
    this.display = [
      0,
      ...shown.filter((c) => this.pinned.has(c)).map((c) => c + 1),
      ...shown.filter((c) => !this.pinned.has(c)).map((c) => c + 1),
    ];
    this.lastPinned = this.display.filter((pos) => this.isPinned(pos)).at(-1);
    this.trendCols = this.display.filter((pos) => pos > 0 && this.trend(pos - 1)).map((pos) => pos - 1);
    this.applyWidths();

    this.headRow.replaceChildren();
    this.filterRow.replaceChildren();
    this.headCells = [];
    for (const pos of this.display) {
      const col = this.cols[pos - 1];
      const cell = el("div", "wg-hcell" + (col?.kind === "number" ? " wg-num" : ""));
      this.place(cell, pos);
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
      cell.append(handle);
      this.headRow.append(cell);
      this.headCells[pos] = cell;

      const filter = el("div", "wg-fcell");
      this.place(filter, pos);
      if (pos > 0) filter.append(this.filterInput(pos - 1, col));
      this.filterRow.append(filter);
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

  /** Gives a cell its column's width, and pins it if its column is pinned. */
  place(cell, pos) {
    cell.style.width = `var(--wg-w${pos})`;
    cell._pos = pos;
    if (this.isPinned(pos)) {
      cell.classList.add("wg-pin");
      cell.style.left = `var(--wg-l${pos})`;
      if (pos === this.lastPinned) cell.classList.add("wg-pin-edge");
    }
  }

  filterInput(col, info) {
    const input = el("input");
    input.type = "text";
    input.spellcheck = false;
    input.value = info.filter;
    input.placeholder = info.kind === "number" ? ">100, 5..10" : info.kind === "bool" ? "true / false" : "contains";
    input.setAttribute("aria-label", `Filter ${info.name}`);
    let timer = 0;
    input.addEventListener("input", () => {
      clearTimeout(timer);
      timer = setTimeout(() => {
        try {
          this.grid.set_filter(col, input.value);
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
    const cell = el("div", "wg-cell" + (col?.kind === "number" ? " wg-num" : "") + (col?.formula ? " wg-calc" : ""));
    this.place(cell, pos);
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

  /** Dragging a header sideways moves its column among its neighbours (pinned or not). */
  dragHeader(e, pos, cell) {
    if (e.button !== 0 || e.target.closest(".wg-resize, .wg-more")) return;
    const col = pos - 1;
    const startX = e.clientX;
    const zone = this.display.filter((p) => p > 0 && this.pinned.has(p - 1) === this.pinned.has(col));
    let dragging = false;
    let before = -1; // the column this one will go in front of; -1 for the end
    cell.setPointerCapture(e.pointerId);
    const move = (m) => {
      if (!dragging && Math.abs(m.clientX - startX) < 6) return;
      dragging = true;
      cell.classList.add("wg-dragging");
      const headBox = this.head.getBoundingClientRect();
      let x = 0;
      before = -1;
      for (const p of zone) {
        const box = this.headCells[p].getBoundingClientRect();
        if (m.clientX < box.left + box.width / 2) {
          before = p - 1;
          x = box.left;
          break;
        }
        x = box.right;
      }
      this.marker.hidden = false;
      this.marker.style.left = `${x - headBox.left + this.head.scrollLeft - 1}px`;
    };
    const done = () => {
      cell.removeEventListener("pointermove", move);
      cell.removeEventListener("pointerup", done);
      cell.removeEventListener("pointercancel", done);
      this.marker.hidden = true;
      cell.classList.remove("wg-dragging");
      if (!dragging) return;
      // The click that follows a drag is not a request to sort.
      this.dragged = true;
      setTimeout(() => (this.dragged = false));
      this.moveColumn(col, before, zone.map((p) => p - 1));
    };
    cell.addEventListener("pointermove", move);
    cell.addEventListener("pointerup", done);
    cell.addEventListener("pointercancel", done);
  }

  /** Moves a column in front of another of its zone, or to the zone's end if `before` is negative. */
  moveColumn(col, before, zone) {
    if (before === col) return;
    const others = zone.filter((c) => c !== col);
    if (others.length === 0) return;
    this.order.splice(this.order.indexOf(col), 1);
    const at = before < 0 ? this.order.indexOf(others.at(-1)) + 1 : this.order.indexOf(before);
    this.order.splice(at, 0, col);
    this.layoutChanged();
  }

  layoutChanged() {
    this.rebuildColumns();
    this.draw();
  }

  setPinned(col, pinned) {
    if (pinned) this.pinned.add(col);
    else this.pinned.delete(col);
    this.layoutChanged();
  }

  setHidden(col, hidden) {
    if (hidden) this.hidden.add(col);
    else this.hidden.delete(col);
    this.layoutChanged();
  }

  /** Column order, pins, hidden columns and widths, by column name, to keep and give back. */
  getState() {
    const name = (col) => this.cols[col].name;
    return {
      order: this.order.map(name),
      hidden: [...this.hidden].map(name),
      pinned: [...this.pinned].map(name),
      widths: Object.fromEntries(this.widths.map((width, pos) => [pos === 0 ? "" : name(pos - 1), width])),
    };
  }

  setState(state) {
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
      if (this.sort.some((s) => s.col === col)) item("Clear sort", () => this.setSort(this.sort.filter((s) => s.col !== col)));
      menu.append(el("hr"));
      item(this.pinned.has(col) ? "Unpin" : "Pin to the left", () => this.setPinned(col, !this.pinned.has(col)));
      item("Hide column", () => this.setHidden(col, true));
      if (info.groupable) {
        if (this.groupCol === col) item("Stop grouping", () => this.setGroup(-1));
        else item(`Group by ${info.name}`, () => this.setGroup(col));
      }
      menu.append(el("hr"));
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
    this.grid.set_sort(sort.map((s) => `${s.col}:${s.desc ? "desc" : "asc"}`).join(","));
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
    this.grid.set_quick_filter(text);
    this.body.scrollTop = 0;
    this.refresh();
  }

  /** Groups by the column at this position, or by nothing if it is negative. */
  setGroup(col) {
    this.grid.set_group(col);
    this.groupCol = col;
    this.body.scrollTop = 0;
    this.refresh();
    this.onGroup(col);
  }

  expandAll(open) {
    this.grid.expand_all(open);
    this.draw();
  }

  /** Opens a group and scrolls to it. */
  revealGroup(key) {
    if (!this.grid.group_is_open(key)) this.grid.toggle_group(key);
    const line = this.grid.group_line(key);
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
   * there are no more. Runs by itself as the person nears the end.
   */
  async fetchMore() {
    if (!this.loadMore || this.loading || this.done) return;
    this.loading = true;
    this.onStatus({ loading: true, done: false });
    const before = this.viewLen;
    let more = false;
    try {
      more = await this.loadMore();
    } catch (error) {
      this.onError(error.message);
    }
    if (this.destroyed) return;
    this.loading = false;
    this.done = !more;
    this.refresh();
    this.onStatus({ loading: false, done: this.done });
    // Still at the end, and the view grew: keep going. If it did not grow
    // (a filter hides the new rows, or groups are closed), wait to be asked.
    if (more && this.viewLen > before && this.nearEnd()) this.fetchMore();
  }

  // ---------------------------------------------------------------- drawing

  /** Applies every change made to the grid since the last call, and redraws. */
  refresh() {
    const started = performance.now();
    const report = JSON.parse(this.grid.update());
    this.draw();
    this.onStatus({
      ...report,
      ms: performance.now() - started,
      rows: this.grid.row_count(),
      filtered: this.grid.filtered_count(),
      groups: this.grid.group_count(),
      selected: this.selected.size,
    });
    if (this.focus) this.onFocus(this.focus);
    return report;
  }

  schedule() {
    cancelAnimationFrame(this.frame);
    this.frame = requestAnimationFrame(() => this.draw());
  }

  format(col, value) {
    if (value === null) return "";
    if (typeof value === "number") return this.numbers[col].format(value);
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
    }
    const first = Math.max(0, Math.floor(this.body.scrollTop / this.rowH) - OVERSCAN);
    const count = Math.ceil(this.body.clientHeight / this.rowH) + 2 * OVERSCAN;
    const lines = JSON.parse(this.grid.rows(first, count));
    const last = first + lines.length;
    const trends = this.trendCols.map((col) => JSON.parse(this.grid.trends(first, lines.length, col, TREND_POINTS)));

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

    const totals = JSON.parse(this.grid.totals());
    const totalsBefore = this.before.get("total");
    const cells = this.footRow._cells;
    for (const pos of this.display) {
      if (pos > 0) this.setCell(cells[pos], pos - 1, totals[pos - 1], totalsBefore?.[pos - 1]);
    }
    for (const col of this.trendCols) this.spark(cells[col + 1], JSON.parse(this.grid.total_trend(col, TREND_POINTS)));
    seen.set("total", totals);
    this.before = seen;
    cells[0].textContent = this.grid.filtered_count() < this.grid.row_count() ? "Total (filtered)" : "Total";
  }

  makeRow() {
    const row = el("div", "wg-row");
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
    row.className =
      "wg-row" +
      (group ? " wg-group" : "") +
      (line % 2 ? " wg-odd" : "") +
      (!group && this.selected.has(data.k) ? " wg-selected" : "");
    const cells = row._cells;
    const first = cells[0];
    const label = group ? `${data.o ? "▾" : "▸"} ${data.g || "(blank)"}  ·  ${data.n.toLocaleString("en-US")}` : data.k;
    if (first._text !== label) first.textContent = first._text = label;
    const was = this.before.get(id);
    const focus = this.focus && this.focus.line === line ? this.focus.pos : -1;
    for (const pos of this.display) {
      const cell = cells[pos];
      cell.classList.toggle("wg-focus", pos === focus);
      if (pos === 0 || cell === this.editing?.cell) continue;
      this.setCell(cell, pos - 1, data.c[pos - 1], was?.[pos - 1]);
    }
    seen.set(id, data.c);
  }

  /** Shows a value in a cell, and flashes it if it is a number that moved. */
  setCell(cell, col, value, was) {
    const text = this.format(col, value);
    if (cell._text !== text) {
      (cell._label ?? cell).textContent = cell._text = text;
      cell.title = value !== null && typeof value === "object" ? value.e : "";
    }
    if (this.formats[this.cols[col].name]?.signed) {
      cell.classList.toggle("wg-neg", typeof value === "number" && value < 0);
      cell.classList.toggle("wg-pos", typeof value === "number" && value > 0);
    }
    if (typeof value === "number" && typeof was === "number" && value !== was) {
      cell.animate(
        [{ backgroundColor: `var(${value > was ? "--wg-up" : "--wg-down"})` }, { backgroundColor: "transparent" }],
        { duration: 700, easing: "ease-out" },
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
    const key = hit.data.k;
    if (e.shiftKey && this.anchor !== null) {
      const [low, high] = [Math.min(this.anchor, hit.line), Math.max(this.anchor, hit.line)];
      const between = JSON.parse(this.grid.rows(low, Math.min(high - low + 1, 20000)));
      if (!(e.metaKey || e.ctrlKey)) this.selected.clear();
      for (const line of between) if (line.k !== undefined) this.selected.add(line.k);
    } else if (e.metaKey || e.ctrlKey) {
      if (!this.selected.delete(key)) this.selected.add(key);
      this.anchor = hit.line;
    } else {
      this.selected.clear();
      this.selected.add(key);
      this.anchor = hit.line;
    }
    this.focusLine(hit.line, hit.pos, hit.data);
    this.draw();
    this.onStatus({ selected: this.selected.size });
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
    this.onFocus(focus);
  }

  /** Why the focused cell has its value, from the engine. */
  explainFocus() {
    const f = this.focus;
    if (!f || f.col < 0) return null;
    const name = this.cols[f.col].name;
    try {
      if (f.kind === "total") return { title: `Total of ${name}`, text: this.grid.explain_total(f.col) };
      if (f.kind === "group") return { title: `${name} for ${f.key || "(blank)"}`, text: this.grid.explain_group(f.key, f.col) };
      return { title: `${name} of ${f.key}`, text: this.grid.explain(f.key, f.col) };
    } catch (error) {
      return { title: name, text: error.message };
    }
  }

  /** Moves the focus to a line and to the column `across` places along the screen. */
  moveFocus(line, across) {
    if (this.viewLen <= 0) return;
    line = Math.min(Math.max(line, 0), this.viewLen - 1);
    const pos = this.display[Math.min(Math.max(across, 0), this.display.length - 1)];
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
    const [data] = JSON.parse(this.grid.rows(line, 1));
    if (!data) return;
    this.focusLine(line, pos, data);
    this.draw();
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
    if (moves[e.key]) {
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
      if (e.key === "Enter") this.stopEdit(true, 1);
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
      try {
        this.grid.set_cell(editing.key, editing.col, text);
      } catch (error) {
        this.onError(error.message);
      }
    }
    if (quiet) return;
    this.body.focus({ preventScroll: true });
    this.refresh();
    if (commit && moveDown && this.focus?.kind === "cell") {
      this.moveFocus(this.focus.line + moveDown, this.display.indexOf(this.focus.pos));
    }
  }

  // -------------------------------------------------------------- selection

  selectedKeys() {
    return [...this.selected];
  }

  /** Removes the selected rows from the grid. */
  removeSelected() {
    for (const key of this.selected) this.grid.remove(key);
    if (this.focus?.kind === "cell" && this.selected.has(this.focus.key)) {
      this.focus = null;
      this.onFocus(null);
    }
    this.selected.clear();
    this.anchor = null;
    this.refresh();
  }
}
