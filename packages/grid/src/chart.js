// A bar for each group's total, and a line for how the grand total moved.
// It draws what the grid hands it, so it follows the grid's sorting,
// filtering and live updates without adding anything up itself.
//
//   const chart = new GroupChart(element, { onPick: (key) => view.revealGroup(key) });
//   chart.update({ title, groups: [{ key, rows, value }], total, trend, format });

const SVG = "http://www.w3.org/2000/svg";
const MOST = 40;

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

export class GroupChart {
  constructor(root, { onPick } = {}) {
    this.root = root;
    this.onPick = onPick ?? (() => {});
    root.classList.add("gc");
    this.figure = el("div", "gc-figure");
    this.total = el("div", "gc-total");
    this.caption = el("div", "gc-caption");
    const svg = document.createElementNS(SVG, "svg");
    svg.setAttribute("class", "gc-line");
    svg.setAttribute("viewBox", "0 0 200 40");
    svg.setAttribute("preserveAspectRatio", "none");
    svg.setAttribute("role", "img");
    this.line = svg.appendChild(document.createElementNS(SVG, "path"));
    this.svg = svg;
    this.figure.append(this.total, this.caption);
    this.bars = el("div", "gc-bars");
    this.note = el("p", "gc-note");
    root.replaceChildren(this.figure, svg, this.bars, this.note);
    this.rows = [];
  }

  /**
   * @param title what is being totalled, such as "sum of net"
   * @param groups [{ key, rows, value }] in the order to show them (value may be null)
   * @param total the grand total, or null
   * @param trend the grand total over recent updates, oldest first, or null
   * @param format a function from a number to text
   * @param hint what to say when there are no groups
   */
  update({ title, groups, total, trend, format, hint }) {
    this.total.textContent = total === null ? "–" : format(total);
    this.caption.textContent = title;

    // How the total moved.
    const drawn = trend && trend.length > 1;
    this.svg.style.visibility = drawn ? "visible" : "hidden";
    if (drawn) {
      const [low, high] = [Math.min(...trend), Math.max(...trend)];
      const step = 200 / (trend.length - 1);
      const y = (v) => (high === low ? 20 : 38 - ((v - low) / (high - low)) * 36).toFixed(1);
      this.line.setAttribute("d", trend.map((v, k) => `${k ? "L" : "M"}${(k * step).toFixed(1)} ${y(v)}`).join(""));
      this.svg.setAttribute("aria-label", `${title} over recent updates: from ${format(trend[0])} to ${format(trend.at(-1))}`);
    }

    // A bar per group. With totals of both signs, bars grow both ways from a
    // middle line; all negative, they grow leftwards from the right.
    const shown = groups.slice(0, MOST);
    const numbers = shown.map((g) => g.value).filter((v) => v !== null);
    const most = Math.max(0, ...numbers.map(Math.abs));
    const both = numbers.some((v) => v < 0) && numbers.some((v) => v > 0);
    const signed = numbers.some((v) => v < 0);
    this.bars.classList.toggle("gc-both", both);
    while (this.rows.length < shown.length) this.rows.push(this.makeRow());
    this.rows.forEach((row, k) => {
      const group = shown[k];
      row.root.hidden = !group;
      if (!group) return;
      row.key = group.key;
      const name = group.key || "(blank)";
      if (row.name.textContent !== name) row.name.textContent = name;
      const value = group.value;
      row.value.textContent = value === null ? "" : format(value);
      const share = value === null || most === 0 ? 0 : Math.abs(value) / most;
      const width = both ? share * 50 : share * 100;
      row.bar.style.width = `${width}%`;
      row.bar.style.left = both ? `${value < 0 ? 50 - width : 50}%` : value < 0 ? `${100 - width}%` : "0";
      row.bar.className = "gc-bar" + (signed ? (value < 0 ? " gc-neg" : " gc-pos") : "");
      row.root.title = `${name}: ${value === null ? "no total" : format(value)} over ${group.rows.toLocaleString("en-US")} rows. Click to open it in the grid.`;
    });
    this.note.textContent =
      groups.length === 0 ? (hint ?? "") : groups.length > MOST ? `…and ${groups.length - MOST} more groups.` : "";
  }

  makeRow() {
    const root = el("button", "gc-row");
    root.type = "button";
    const row = { root, key: "", name: el("span", "gc-name"), track: el("span", "gc-track"), bar: el("i", "gc-bar"), value: el("span", "gc-value") };
    row.track.append(row.bar);
    root.append(row.name, row.track, row.value);
    root.addEventListener("click", () => this.onPick(row.key));
    this.bars.append(root);
    return row;
  }
}
