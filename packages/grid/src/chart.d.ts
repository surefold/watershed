export interface ChartData {
  title: string;
  groups: { key: string; rows: number; value: number | null }[];
  total: number | null;
  /** The total over recent recalculations, oldest first. */
  trend: number[] | null;
  format: (value: number) => string;
  /** What to say when there are no groups. */
  hint?: string;
}

/** A bar for each group's total, and a line for how the grand total moved. */
export declare class GroupChart {
  constructor(root: HTMLElement, options?: { onPick?: (key: string) => void });
  update(data: ChartData): void;
}
