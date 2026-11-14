//! What the examples that trade on daily prices share: reading the prices,
//! the trading rules as a graph, and the settings to try.

// Each example uses some of this, not all.
#![allow(dead_code)]
// Days and shares are numbers here, used to find several things at once.
#![allow(clippy::needless_range_loop)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rayon::prelude::*;
use watershed_core::prelude::*;
use watershed_core::{Expr, IntoExpr, when};
use watershed_gpu::Scenarios;

/// Prices from this day on are used (year, month, day as one number).
pub const START: u32 = 20100101;
/// Settings are chosen on the days before this one and tested from it on.
pub const SPLIT: u32 = 20200101;
/// Days at the start with no trading, while the averages settle and each
/// share's usual daily movement is measured.
pub const WARM_UP: usize = 250;
/// What a trade costs when costs are charged: 5 hundredths of a percent of
/// the amount traded.
pub const COST: f64 = 0.0005;
pub const DAYS_A_YEAR: f64 = 252.0;

/// A small deterministic random number generator, so every run is the same.
pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    pub fn unit(&mut self) -> f64 {
        ((self.next() >> 11) as f64 + 1.0) / (1u64 << 53) as f64
    }
    pub fn normal(&mut self) -> f64 {
        (-2.0 * self.unit().ln()).sqrt() * (std::f64::consts::TAU * self.unit()).cos()
    }
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, self.below(i + 1));
        }
    }
}

/// Closing prices: `prices[share][day]`.
pub struct History {
    pub prices: Vec<Vec<f64>>,
    /// The first day that is tested on, not chosen on.
    pub split: usize,
    pub what: String,
}

impl History {
    pub fn days(&self) -> usize {
        self.prices[0].len()
    }
    /// How much each share rose on `day`, as a fraction.
    pub fn rise(&self, share: usize, day: usize) -> f64 {
        self.prices[share][day] / self.prices[share][day - 1] - 1.0
    }
}

// ---- Reading Stooq's files ----

struct Series {
    /// (day, closing price, shares traded), in day order.
    rows: Vec<(u32, f64, f64)>,
}

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            // Funds that hold many shares would count those shares twice.
            if !path.to_string_lossy().to_lowercase().contains("etf") {
                files_under(&path, out)?;
            }
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("txt" | "csv")
        ) {
            out.push(path);
        }
    }
    Ok(())
}

/// One file of daily prices, in either form Stooq gives them: the bulk
/// download (`<TICKER>,<PER>,<DATE>,<TIME>,<OPEN>,<HIGH>,<LOW>,<CLOSE>,<VOL>,...`)
/// or one share's history (`Date,Open,High,Low,Close,Volume`).
fn read_series(path: &Path) -> Option<Series> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    let bulk = lines.next()?.starts_with('<');
    let mut rows = Vec::new();
    for line in lines {
        let c: Vec<&str> = line.split(',').collect();
        let (day, close, traded) = if bulk {
            if c.len() < 9 || c[1] != "D" {
                continue;
            }
            (c[2].to_string(), c[7], c[8])
        } else {
            if c.len() < 5 {
                continue;
            }
            (
                c[0].replace('-', ""),
                c[4],
                c.get(5).copied().unwrap_or("0"),
            )
        };
        let (Ok(day), Ok(close)) = (day.parse::<u32>(), close.parse::<f64>()) else {
            continue;
        };
        if day >= START && close.is_finite() && close > 0.0 {
            rows.push((day, close, traded.parse().unwrap_or(0.0)));
        }
    }
    rows.sort_by_key(|r| r.0);
    (rows.len() > WARM_UP).then_some(Series { rows })
}

pub fn read_history(dir: &Path, shares: usize) -> Result<History, Box<dyn std::error::Error>> {
    let mut files = Vec::new();
    files_under(dir, &mut files)?;
    let all: Vec<Series> = files.par_iter().filter_map(|f| read_series(f)).collect();

    // The days the market was open: those most shares have a price for.
    let mut seen: HashMap<u32, usize> = HashMap::new();
    for series in &all {
        for row in &series.rows {
            *seen.entry(row.0).or_default() += 1;
        }
    }
    let most = seen.values().copied().max().unwrap_or(0);
    let mut days: Vec<u32> = seen
        .into_iter()
        .filter(|&(_, count)| 2 * count >= most)
        .map(|(day, _)| day)
        .collect();
    days.sort_unstable();
    if days.len() < 3 * WARM_UP {
        return Err(format!(
            "{} files under {} hold only {} days of prices since {START}",
            files.len(),
            dir.display(),
            days.len()
        )
        .into());
    }

    // Shares with a price from the first week to the last, on nearly every
    // day, that are not pennies, and with no day so wild it must be a
    // mistake in the data. Of those, the most traded in the first year:
    // a choice that could have been made at the time.
    let (first_week, last_week) = (days[5], days[days.len() - 6]);
    let mut kept: Vec<(f64, Vec<f64>)> = all
        .par_iter()
        .filter_map(|series| {
            let rows = &series.rows;
            if rows[0].0 > first_week || rows[rows.len() - 1].0 < last_week {
                return None;
            }
            // Each market day's price, or the one before where a day is missing.
            let mut prices = Vec::with_capacity(days.len());
            let (mut at, mut missing) = (0, 0);
            for &day in &days {
                while at + 1 < rows.len() && rows[at + 1].0 <= day {
                    at += 1;
                }
                missing += usize::from(rows[at].0 != day);
                prices.push(rows[at].1);
            }
            let wild = prices
                .windows(2)
                .any(|p| p[1] > 3.0 * p[0] || 3.0 * p[1] < p[0]);
            if 50 * missing > days.len() || prices[0] < 5.0 || wild {
                return None;
            }
            let mut traded: Vec<f64> = rows
                .iter()
                .take_while(|r| r.0 <= days[WARM_UP])
                .map(|r| r.1 * r.2)
                .collect();
            traded.sort_by(f64::total_cmp);
            Some((traded[traded.len() / 2], prices))
        })
        .collect();
    kept.sort_by(|a, b| b.0.total_cmp(&a.0));
    let found = kept.len();
    kept.truncate(shares);
    if kept.len() < 20 {
        return Err(format!("only {found} shares have prices for the whole period").into());
    }
    let split = days.partition_point(|&day| day < SPLIT);
    if split <= WARM_UP + 250 || days.len() - split < 250 {
        return Err(format!("the prices do not reach a year either side of {SPLIT}").into());
    }
    Ok(History {
        what: format!(
            "{} shares (the most traded of {found} with prices throughout, from {} files), {} to {}",
            kept.len(),
            files.len(),
            days[0],
            days[days.len() - 1]
        ),
        prices: kept.into_iter().map(|(_, prices)| prices).collect(),
        split,
    })
}

/// Prices that wander at random: each day a share moves partly with the
/// market and partly on its own, and no day says anything about the next.
pub fn made_up_history(shares: usize) -> History {
    let days = 3_750;
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let market: Vec<f64> = (0..days).map(|_| rng.normal()).collect();
    let prices = (0..shares)
        .map(|_| {
            let daily = (0.15 + 0.35 * rng.unit()) / DAYS_A_YEAR.sqrt();
            let with_market = 0.3 + 0.5 * rng.unit();
            let own = (1.0 - with_market * with_market).sqrt();
            let mut price = 20.0 + 200.0 * rng.unit();
            let mut path = Vec::with_capacity(days);
            for &m in &market {
                let moved = with_market * m + own * rng.normal();
                price *= (0.07 / DAYS_A_YEAR + daily * moved - daily * daily / 2.0).exp();
                path.push(price);
            }
            path
        })
        .collect();
    History {
        prices,
        split: days * 2 / 3,
        what: format!("{shares} made-up shares that wander at random, {days} days"),
    }
}

/// The same history with its days in a random order: every share moves by
/// the same amounts, and they still move together on the same day, but what
/// came before a day no longer has anything to do with it. The earlier and
/// the later years are each shuffled among themselves.
pub fn shuffled(history: &History, rng: &mut Rng) -> History {
    let days = history.days();
    let mut order: Vec<usize> = (1..days).collect();
    let (warm, rest) = order.split_at_mut(WARM_UP);
    let (earlier, later) = rest.split_at_mut(history.split - 1 - WARM_UP);
    rng.shuffle(warm);
    rng.shuffle(earlier);
    rng.shuffle(later);
    let prices = history
        .prices
        .iter()
        .map(|was| {
            let mut price = was[0];
            let mut path = vec![price];
            for &day in &order {
                price *= was[day] / was[day - 1];
                path.push(price);
            }
            path
        })
        .collect();
    History {
        prices,
        split: history.split,
        what: String::new(),
    }
}

// ---- The rules, as graphs ----

/// Which rule to try. Every rule holds the market, each share equally, and
/// leans away from that by its gain; with a gain of zero it is the market.
#[derive(Clone, Copy, PartialEq)]
pub enum Rule {
    /// Each share against its own past: a quick average of its price
    /// against a slow one.
    Trend,
    /// Each share against the rest: how it has been rising against how the
    /// market has.
    Relative,
    /// The whole market against its own past: hold less of everything when
    /// the market has lately been jumpier than usual.
    Calm,
    /// Each share against the rest: hold more of those that have been
    /// quieter than most.
    Quiet,
    /// Each share against the rest over the past year, leaving out the last
    /// month: hold more of the year's winners.
    Year,
    /// Pairs of shares that moved together before trading started: when a
    /// pair drifts apart, bet that it closes up. Unlike the others this can
    /// hold less than nothing of a share.
    Pairs,
}

impl Rule {
    pub const ALL: [Rule; 6] = [
        Rule::Trend,
        Rule::Relative,
        Rule::Calm,
        Rule::Quiet,
        Rule::Year,
        Rule::Pairs,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Rule::Trend => "trend",
            Rule::Relative => "relative",
            Rule::Calm => "calm",
            Rule::Quiet => "quiet",
            Rule::Year => "year",
            Rule::Pairs => "pairs",
        }
    }

    pub fn named(name: &str) -> Option<Rule> {
        Rule::ALL.into_iter().find(|rule| rule.name() == name)
    }
}

/// The trading days in a month, in half a year and in a year.
const MONTH: usize = 21;
const HALF_YEAR: usize = 126;
const YEAR: usize = 252;

/// A rule, and what is settled about it before trading starts. It is made
/// from the history as it happened and used for every history of that
/// shape, so that all of them are run by one plan.
pub struct Setup {
    pub rule: Rule,
    /// For the pairs rule: the pairs.
    pairs: Vec<(usize, usize)>,
}

impl Setup {
    pub fn new(history: &History, rule: Rule) -> Setup {
        let pairs = match rule {
            Rule::Pairs => pairs_of(history),
            _ => Vec::new(),
        };
        Setup { rule, pairs }
    }
}

/// How much each share's logarithm moved each day of the warm-up, and the
/// usual size of that movement.
fn warm_up_moves(prices: &[f64]) -> (Vec<f64>, f64) {
    let moves: Vec<f64> = prices[..=WARM_UP]
        .windows(2)
        .map(|w| (w[1] / w[0]).ln())
        .collect();
    let mean = moves.iter().sum::<f64>() / moves.len() as f64;
    let usual = (moves.iter().map(|m| (m - mean).powi(2)).sum::<f64>() / moves.len() as f64)
        .sqrt()
        .max(1e-6);
    (moves.iter().map(|m| m - mean).collect(), usual)
}

/// Pairs of shares, each share in at most one: the pairs whose daily moves
/// went together most closely during the warm-up, closest first.
fn pairs_of(history: &History) -> Vec<(usize, usize)> {
    let moves: Vec<(Vec<f64>, f64)> = history.prices.iter().map(|p| warm_up_moves(p)).collect();
    let shares = moves.len();
    let mut together: Vec<(f64, usize, usize)> = (0..shares)
        .into_par_iter()
        .flat_map_iter(|a| {
            let moves = &moves;
            (a + 1..shares).map(move |b| {
                let dot: f64 = moves[a].0.iter().zip(&moves[b].0).map(|(x, y)| x * y).sum();
                (dot / (WARM_UP as f64 * moves[a].1 * moves[b].1), a, b)
            })
        })
        .collect();
    together.sort_by(|x, y| y.0.total_cmp(&x.0).then((x.1, x.2).cmp(&(y.1, y.2))));
    let mut taken = vec![false; shares];
    let mut pairs = Vec::new();
    for (_, a, b) in together {
        if !taken[a] && !taken[b] {
            taken[a] = true;
            taken[b] = true;
            pairs.push((a, b));
        }
    }
    pairs
}

/// The data a rule's plan shares between scenarios, as columns of one value
/// a day (day 0 is not used). Column 0 is how much the market, every share
/// held equally, rose that day. Then:
///
/// - trend: for each share, how much it rose, and the logarithm of its price;
/// - relative, quiet: for each share, how much it rose;
/// - calm: nothing more;
/// - year: for each share, how much it rose, and how far ahead of the other
///   shares it was over the past year and over the past half year, each
///   leaving out the last month, in units of its usual movement over that
///   long (zero until there is a year of prices);
/// - pairs: for each pair, how far apart the logarithms of the two prices
///   are, and how much more the first rose than the second.
fn columns(history: &History, setup: &Setup) -> Vec<Vec<f64>> {
    let (shares, days) = (history.prices.len(), history.days());
    let rises = |i: usize| -> Vec<f64> {
        (0..days)
            .map(|day| if day == 0 { 0.0 } else { history.rise(i, day) })
            .collect()
    };
    let logs = |i: usize| -> Vec<f64> { history.prices[i].iter().map(|p| p.ln()).collect() };
    let mut market = vec![0.0; days];
    for day in 1..days {
        market[day] = (0..shares).map(|i| history.rise(i, day)).sum::<f64>() / shares as f64;
    }
    let mut columns = vec![market];
    match setup.rule {
        Rule::Calm => {}
        Rule::Relative | Rule::Quiet => columns.extend((0..shares).map(rises)),
        Rule::Trend => {
            for i in 0..shares {
                columns.push(rises(i));
                columns.push(logs(i));
            }
        }
        Rule::Year => {
            // How far each share's logarithm moved over a stretch ending a
            // month ago, in units of its usual movement over that long.
            let over = |from: usize| -> Vec<Vec<f64>> {
                let mut moved: Vec<Vec<f64>> = (0..shares)
                    .map(|i| {
                        let (log, usual) = (logs(i), warm_up_moves(&history.prices[i]).1);
                        let scale = usual * ((from - MONTH) as f64).sqrt();
                        (0..days)
                            .map(|day| match day >= YEAR {
                                true => (log[day - MONTH] - log[day - from]) / scale,
                                false => 0.0,
                            })
                            .collect()
                    })
                    .collect();
                // Against the other shares: less the average of them all.
                for day in YEAR..days {
                    let mean = moved.iter().map(|m| m[day]).sum::<f64>() / shares as f64;
                    moved.iter_mut().for_each(|m| m[day] -= mean);
                }
                moved
            };
            let (year, half) = (over(YEAR), over(HALF_YEAR));
            for (i, (year, half)) in year.into_iter().zip(half).enumerate() {
                columns.push(rises(i));
                columns.push(year);
                columns.push(half);
            }
        }
        Rule::Pairs => {
            for &(a, b) in &setup.pairs {
                let (log_a, log_b, rise_a, rise_b) = (logs(a), logs(b), rises(a), rises(b));
                columns.push(log_a.iter().zip(&log_b).map(|(x, y)| x - y).collect());
                columns.push(rise_a.iter().zip(&rise_b).map(|(x, y)| x - y).collect());
            }
        }
    }
    columns
}

/// The values of a rule's plan's shared inputs for `history`: give them to
/// [`Scenarios::set_shared`] to run the same plan on another history with
/// as many shares and days, and the same first day and warm-up.
pub fn data_for(history: &History, setup: &Setup) -> Vec<f64> {
    columns(history, setup).concat()
}

/// One node adding up `nodes`: eight at a time, then those eight at a time.
pub fn sum(g: &mut Graph, name: &str, mut nodes: Vec<Node<f64>>) -> Result<Node<f64>, CalcError> {
    let mut round = 0;
    while nodes.len() > 1 {
        round += 1;
        let mut next = Vec::with_capacity(nodes.len() / 8 + 1);
        for (k, group) in nodes.chunks(8).enumerate() {
            let mut all = group[0].into_expr();
            for &node in &group[1..] {
                all = all + node;
            }
            next.push(g.define(&format!("{name}_{round}_{k}"), all)?);
        }
        nodes = next;
    }
    Ok(nodes[0])
}

/// What a rule is built from.
struct Parts<'a> {
    g: &'a mut Graph,
    /// The settings. What the first two mean depends on the rule.
    first: Input<f64>,
    second: Input<f64>,
    gain: Input<f64>,
    cost: Input<f64>,
    /// The shared data: see `columns`.
    data: &'a [Input<f64>],
    shares: usize,
    days: usize,
    /// Each day's profits, to be added up: one for each share, or for each
    /// pair, or one for the whole market.
    profit_on: Vec<Vec<Node<f64>>>,
}

impl Parts<'_> {
    fn at(&self, column: usize, day: usize) -> Input<f64> {
        self.data[column * self.days + day]
    }

    /// An average that moves `fraction` of the way to `value` each day:
    /// today's, given yesterday's. It starts from nothing.
    fn average(
        &mut self,
        name: &str,
        before: Option<Node<f64>>,
        value: impl IntoExpr<f64>,
        fraction: Input<f64>,
    ) -> Result<Node<f64>, CalcError> {
        let value = value.into_expr();
        match before {
            None => self.g.define(name, value * fraction),
            Some(a) => self.g.define(name, a - fraction * (a - value)),
        }
    }

    /// Holds the usual amount, 1, of something, more or less by how far the
    /// rule leans: between none and twice as much. Adds the day's profit on
    /// what was held overnight, less the cost of changing it, times `worth`.
    fn hold(
        &mut self,
        name: &str,
        day: usize,
        lean: Expr<f64>,
        held: &mut Option<Node<f64>>,
        rise: Input<f64>,
        worth: f64,
    ) -> Result<(), CalcError> {
        let hold = self.g.define(
            &format!("h{name}"),
            (lean * self.gain + 1.0).max(0.0).min(2.0),
        )?;
        if let Some(before) = held.replace(hold) {
            let profit = before * rise - (hold - before).abs() * self.cost;
            let profit = match worth == 1.0 {
                true => profit,
                false => profit * worth,
            };
            let node = self.g.define(&format!("p{name}"), profit)?;
            self.profit_on[day].push(node);
        }
        Ok(())
    }

    /// Trend and relative: one share after another.
    fn share_by_share(&mut self, history: &History, rule: Rule) -> Result<(), CalcError> {
        // For the relative rule: an average of how much the market has been
        // rising a day.
        let mut market: Vec<Option<Node<f64>>> = vec![None; self.days];
        if rule == Rule::Relative {
            for day in 1..self.days {
                let (rise, quick) = (self.at(0, day), self.first);
                market[day] =
                    Some(self.average(&format!("m{day}"), market[day - 1], rise, quick)?);
            }
        }
        for (i, prices) in history.prices.iter().enumerate() {
            // The share's usual daily movement, measured before trading
            // starts, so that a jumpy share and a quiet one are judged
            // alike. It and the first day's price are constants: `shuffled`
            // leaves both as they are.
            let usual = warm_up_moves(prices).1;
            let first_log = prices[0].ln();
            let (quick, slow) = (self.first, self.second);
            let mut averages: Option<(Node<f64>, Node<f64>)> = None;
            let mut rising: Option<Node<f64>> = None;
            let mut held: Option<Node<f64>> = None;
            for day in 1..self.days {
                // Which way the rule leans on this share today, in units of
                // the share's usual daily movement.
                let lean: Expr<f64> = match rule {
                    Rule::Trend => {
                        // Averages are of the logarithm of the price, so
                        // that a rise from 10 to 11 counts the same as one
                        // from 100 to 110. Each moves a fraction of the way
                        // to today's price, starting from the first day's.
                        let price = self.at(2 + 2 * i, day);
                        let (q, s) = match averages {
                            None => (
                                self.g.define(
                                    &format!("q{i}_{day}"),
                                    (price - first_log) * quick + first_log,
                                )?,
                                self.g.define(
                                    &format!("s{i}_{day}"),
                                    (price - first_log) * slow + first_log,
                                )?,
                            ),
                            Some((q, s)) => (
                                self.g
                                    .define(&format!("q{i}_{day}"), q - quick * (q - price))?,
                                self.g
                                    .define(&format!("s{i}_{day}"), s - slow * (s - price))?,
                            ),
                        };
                        averages = Some((q, s));
                        (q - s) * (1.0 / usual)
                    }
                    _ => {
                        // The average moves a fraction of the way to today's rise.
                        let rise = self.at(1 + i, day);
                        let a = self.average(&format!("a{i}_{day}"), rising, rise, quick)?;
                        rising = Some(a);
                        (a - market[day].expect("made above")) * (1.0 / usual)
                    }
                };
                if day >= WARM_UP {
                    let rise = match rule {
                        Rule::Trend => self.at(1 + 2 * i, day),
                        _ => self.at(1 + i, day),
                    };
                    self.hold(&format!("{i}_{day}"), day, lean, &mut held, rise, 1.0)?;
                }
            }
        }
        Ok(())
    }

    /// Calm: the whole market at once. The first setting is how quick the
    /// average of the market's recent jumpiness is, the second how slow the
    /// one it is compared with.
    fn calm(&mut self) -> Result<(), CalcError> {
        let (mut lately, mut usually) = (None, None);
        let mut held = None;
        for day in 1..self.days {
            let rise = self.at(0, day);
            // Jumpiness is the square of a day's rise or fall.
            let (quick, slow) = (self.first, self.second);
            let now = self.average(&format!("jq{day}"), lately, rise * rise, quick)?;
            let then = self.average(&format!("js{day}"), usually, rise * rise, slow)?;
            (lately, usually) = (Some(now), Some(then));
            if day >= WARM_UP {
                // Twice as jumpy as usual leans all the way out.
                let lean = -(now / then) + 1.0;
                let worth = self.shares as f64;
                self.hold(&format!("{day}"), day, lean, &mut held, rise, worth)?;
            }
        }
        Ok(())
    }

    /// Quiet: every share each day, since each is judged against the rest.
    /// The first setting is how quick the average of a share's jumpiness is.
    fn quiet(&mut self) -> Result<(), CalcError> {
        let mut lately: Vec<Option<Node<f64>>> = vec![None; self.shares];
        let mut held: Vec<Option<Node<f64>>> = vec![None; self.shares];
        for day in 1..self.days {
            let quick = self.first;
            let mut today = Vec::with_capacity(self.shares);
            for i in 0..self.shares {
                let rise = self.at(1 + i, day);
                let now = self.average(&format!("j{i}_{day}"), lately[i], rise * rise, quick)?;
                lately[i] = Some(now);
                today.push(now);
            }
            if day >= WARM_UP {
                let all = sum(self.g, &format!("ja{day}"), today.clone())?;
                for (i, now) in today.into_iter().enumerate() {
                    // As jumpy as the average share leans nowhere; not
                    // jumpy at all leans all the way in.
                    let lean = -(now * (self.shares as f64) / all) + 1.0;
                    let rise = self.at(1 + i, day);
                    self.hold(&format!("{i}_{day}"), day, lean, &mut held[i], rise, 1.0)?;
                }
            }
        }
        Ok(())
    }

    /// Year: the leaning is data. The first setting is how much of it is
    /// the half year's and not the year's, from 0 to 1.
    fn year(&mut self) -> Result<(), CalcError> {
        for i in 0..self.shares {
            let mut held = None;
            for day in WARM_UP..self.days {
                let (rise, year, half) = (
                    self.at(1 + 3 * i, day),
                    self.at(2 + 3 * i, day),
                    self.at(3 + 3 * i, day),
                );
                let lean = (half - year) * self.first + year;
                self.hold(&format!("{i}_{day}"), day, lean, &mut held, rise, 1.0)?;
            }
        }
        Ok(())
    }

    /// Pairs: the market, and a bet on each pair. The first setting is how
    /// far apart, in the pair's usual distances, a pair must drift before a
    /// bet is made; the second how quick the averages of where the pair
    /// usually is, and of how far from there it usually strays, are.
    fn pairs(&mut self, pairs: usize) -> Result<(), CalcError> {
        let (enter, quick) = (self.first, self.second);
        // The bets are sized so that all of them together are as much money
        // as the shares held.
        let worth = self.shares as f64 / pairs as f64;
        for pair in 0..pairs {
            let mut middle: Option<Node<f64>> = None;
            let mut strays: Option<Node<f64>> = None;
            let mut bet: Option<Node<f64>> = None;
            for day in 1..self.days {
                let (apart, rose_more) = (self.at(1 + 2 * pair, day), self.at(2 + 2 * pair, day));
                let name = format!("{pair}_{day}");
                // How far the pair is from where it usually is, as of
                // yesterday; then yesterday's "usually" takes today in.
                let Some(usual_place) = middle else {
                    middle = Some(self.g.define(&format!("pm{name}"), apart * 1.0)?);
                    continue;
                };
                let off = self.g.define(&format!("po{name}"), apart - usual_place)?;
                middle = Some(self.g.define(
                    &format!("pm{name}"),
                    usual_place - quick * (usual_place - apart),
                )?);
                let strayed = strays;
                strays = Some(self.average(&format!("ps{name}"), strays, off * off, quick)?);
                let (true, Some(strayed)) = (day >= WARM_UP, strayed) else {
                    continue;
                };
                // Further off than the setting allows: bet on the pair
                // closing up, against whichever is ahead. Otherwise keep a
                // bet until the pair is back where it usually is, and make
                // none.
                let keep = match bet {
                    Some(bet) => when((bet * off).lt(0.0)).then(bet).otherwise(0.0),
                    None => 0.0.into_expr(),
                };
                let now = self.g.define(
                    &format!("pb{name}"),
                    when((off * off).gt(strayed * enter * enter))
                        .then(when(off.gt(0.0)).then(-1.0).otherwise(1.0))
                        .otherwise(keep),
                )?;
                if let Some(before) = bet.replace(now) {
                    // A bet is two trades: one share bought, the other sold.
                    // The cost is paid whichever way the bet is made.
                    let node = self.g.define(
                        &format!("pp{name}"),
                        (before * rose_more * self.gain
                            - (now - before).abs() * self.cost * 2.0 * self.gain.abs())
                            * worth,
                    )?;
                    self.profit_on[day].push(node);
                }
            }
        }
        // The market itself, every share held equally.
        for day in WARM_UP + 1..self.days {
            let node = self
                .g
                .define(&format!("mk{day}"), self.at(0, day) * (self.shares as f64))?;
            self.profit_on[day].push(node);
        }
        Ok(())
    }
}

/// The rule on every share and every day of `history`, as a plan whose
/// scenarios are settings (see `inputs_for`). Every share's profit is
/// counted whole, so divide by the number of shares. Its outputs are either
/// each day's profit, for every day after the warm-up, or the sum of each
/// day's profit and of its square, for the earlier years and then the later.
pub fn plan_for(
    history: &History,
    setup: &Setup,
    every_day: bool,
) -> Result<Scenarios, Box<dyn std::error::Error>> {
    let (shares, days) = (history.prices.len(), history.days());
    let mut g = Graph::new();
    // The settings. Their values here do not matter: every scenario sets them.
    let first = g.input("first", 0.1)?;
    let second = g.input("second", 0.01)?;
    let gain = g.input("gain", 0.0)?;
    let cost = g.input("cost", 0.0)?;

    // The prices, as inputs shared by every scenario, so that the same plan
    // can be run on another history of the same shape.
    let data = data_for(history, setup);
    g.reserve(3 * data.len(), 6 * data.len());
    let data: Vec<Input<f64>> = data
        .into_iter()
        .enumerate()
        .map(|(n, x)| g.input(&format!("x{n}"), x))
        .collect::<Result<_, _>>()?;

    let mut parts = Parts {
        g: &mut g,
        first,
        second,
        gain,
        cost,
        data: &data,
        shares,
        days,
        profit_on: vec![Vec::new(); days],
    };
    match setup.rule {
        Rule::Trend | Rule::Relative => parts.share_by_share(history, setup.rule)?,
        Rule::Calm => parts.calm()?,
        Rule::Quiet => parts.quiet()?,
        Rule::Year => parts.year()?,
        Rule::Pairs => parts.pairs(setup.pairs.len())?,
    }
    let profit_on = parts.profit_on;

    let mut outputs = Vec::new();
    let mut profits = profit_on
        .into_iter()
        .enumerate()
        .skip(WARM_UP + 1)
        .peekable();
    if every_day {
        for (day, profits) in profits {
            outputs.push(sum(&mut g, &format!("d{day}"), profits)?.id());
        }
    } else {
        for (name, until) in [("earlier", history.split), ("later", days)] {
            let (mut each, mut squared) = (Vec::new(), Vec::new());
            while let Some((day, profits)) = profits.next_if(|(day, _)| *day < until) {
                let all = sum(&mut g, &format!("d{day}"), profits)?;
                squared.push(g.define(&format!("dd{day}"), all * all)?);
                each.push(all);
            }
            outputs.push(sum(&mut g, name, each)?.id());
            outputs.push(sum(&mut g, &format!("{name}_squared"), squared)?.id());
        }
    }
    g.stabilize();
    let vary = [first.id(), second.id(), gain.id(), cost.id()];
    let shared: Vec<_> = data.iter().map(|x| x.id()).collect();
    Ok(Scenarios::with_shared(&g, &vary, &shared, &outputs)?)
}

// ---- The settings to try ----

/// One variant of a rule. What `first` and `second` are depends on the rule:
/// see `describe`.
#[derive(Clone, Copy)]
pub struct Settings {
    pub first: f64,
    pub second: f64,
    /// How far the rule leans. Zero is the market.
    pub gain: f64,
    pub cost: f64,
}

pub fn settings_to_try(rule: Rule) -> Vec<Settings> {
    let steps = |from: f64, to: f64, n: usize| -> Vec<f64> {
        (0..n)
            .map(|k| from * (to / from).powf(k as f64 / (n - 1) as f64))
            .collect()
    };
    let gains = |from: f64, to: f64, n: usize| {
        let mut gains = vec![0.0];
        for gain in steps(from, to, n) {
            gains.extend([gain, -gain]);
        }
        gains
    };
    // Every pairing of a first and a second setting, and the gains.
    let (pairings, gains): (Vec<(f64, f64)>, Vec<f64>) = match rule {
        Rule::Trend => (
            steps(1.0, 40.0, 10)
                .into_iter()
                .flat_map(|quick| {
                    steps(20.0, 400.0, 10)
                        .into_iter()
                        .map(move |slow| (quick, slow))
                })
                .filter(|(quick, slow)| *slow >= 2.0 * quick)
                .collect(),
            gains(0.005, 2.0, 20),
        ),
        Rule::Relative => (
            steps(1.0, 120.0, 30)
                .into_iter()
                .map(|days| (days, days))
                .collect(),
            gains(0.02, 20.0, 30),
        ),
        Rule::Calm => (
            steps(2.0, 40.0, 8)
                .into_iter()
                .flat_map(|quick| {
                    steps(40.0, 400.0, 6)
                        .into_iter()
                        .map(move |slow| (quick, slow))
                })
                .collect(),
            gains(0.02, 4.0, 20),
        ),
        Rule::Quiet => (
            steps(5.0, 250.0, 16)
                .into_iter()
                .map(|days| (days, days))
                .collect(),
            gains(0.02, 4.0, 30),
        ),
        Rule::Year => (
            [0.0, 0.25, 0.5, 0.75, 1.0]
                .into_iter()
                .map(|half| (half, 0.0))
                .collect(),
            gains(0.005, 2.0, 30),
        ),
        Rule::Pairs => (
            [0.5, 1.0, 1.5, 2.0, 2.5, 3.0]
                .into_iter()
                .flat_map(|enter| {
                    steps(5.0, 120.0, 6)
                        .into_iter()
                        .map(move |days| (enter, days))
                })
                .collect(),
            gains(0.05, 2.0, 10),
        ),
    };
    let mut all = Vec::new();
    for cost in [COST, 0.0] {
        for &(first, second) in &pairings {
            for &gain in &gains {
                all.push(Settings {
                    first,
                    second,
                    gain,
                    cost,
                });
            }
        }
    }
    all
}

/// The settings as a rule's plan takes them: one scenario after another.
pub fn inputs_for(rule: Rule, settings: &[Settings]) -> Vec<f64> {
    // An average that covers half the distance in `d` days moves this
    // fraction of the way each day.
    let fraction = |days: f64| 1.0 - 0.5f64.powf(1.0 / days);
    settings
        .iter()
        .flat_map(|s| {
            let (first, second) = match rule {
                Rule::Trend | Rule::Relative | Rule::Calm | Rule::Quiet => {
                    (fraction(s.first), fraction(s.second))
                }
                Rule::Year => (s.first, 0.0),
                Rule::Pairs => (s.first, fraction(s.second)),
            };
            [first, second, s.gain, s.cost]
        })
        .collect()
}

/// A variant of a rule, in words.
pub fn describe(rule: Rule, s: &Settings) -> String {
    let what = match rule {
        Rule::Trend => format!("averages of {:.0} and {:.0} days", s.first, s.second),
        Rule::Relative => format!("an average over {:.0} days", s.first),
        Rule::Calm => format!("jumpiness over {:.0} days against {:.0}", s.first, s.second),
        Rule::Quiet => format!("jumpiness over {:.0} days", s.first),
        Rule::Year => format!(
            "{:.0}% the past half year, the rest the past year",
            100.0 * s.first
        ),
        Rule::Pairs => format!(
            "pairs {:.1} usual distances apart, usual over {:.0} days",
            s.first, s.second
        ),
    };
    let does = match (rule, s.gain) {
        (_, 0.0) => "hold the market",
        (Rule::Trend, g) if g > 0.0 => "hold more of what has been rising",
        (Rule::Trend, _) => "hold more of what has been falling",
        (Rule::Relative, g) if g > 0.0 => "hold more of what has been ahead of the market",
        (Rule::Relative, _) => "hold more of what has been behind the market",
        (Rule::Calm, g) if g > 0.0 => "hold less when the market has been jumpy",
        (Rule::Calm, _) => "hold more when the market has been jumpy",
        (Rule::Quiet, g) if g > 0.0 => "hold more of the quiet shares",
        (Rule::Quiet, _) => "hold more of the jumpy shares",
        (Rule::Year, g) if g > 0.0 => "hold more of the winners",
        (Rule::Year, _) => "hold more of the losers",
        (Rule::Pairs, g) if g > 0.0 => "bet that pairs close up",
        (Rule::Pairs, _) => "bet that pairs drift further",
    };
    format!("{}, {what}, gain {:+.3} ({does})", rule.name(), s.gain)
}

pub struct Runner {
    #[cfg(feature = "gpu")]
    gpu: watershed_gpu::Gpu,
}

impl Runner {
    pub fn new() -> Result<Runner, Box<dyn std::error::Error>> {
        Ok(Runner {
            #[cfg(feature = "gpu")]
            gpu: watershed_gpu::Gpu::new()?,
        })
    }

    pub fn name(&self) -> &'static str {
        if cfg!(feature = "gpu") { "GPU" } else { "CPU" }
    }

    pub fn run(
        &self,
        plan: &Scenarios,
        inputs: &[f64],
    ) -> Result<Vec<f64>, Box<dyn std::error::Error>> {
        #[cfg(feature = "gpu")]
        return Ok(self.gpu.run(plan, inputs)?);
        #[cfg(not(feature = "gpu"))]
        return Ok(plan.run_cpu(inputs));
    }
}

pub fn time(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s < 1.0 {
        format!("{:.0} ms", s * 1e3)
    } else {
        format!("{s:.1} s")
    }
}
