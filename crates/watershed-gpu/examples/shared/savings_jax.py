"""The savings model of the `liabilities` example written by hand for JAX: a
baseline for what a program written for this one model does on the same GPU.

It is the same model (lifelib's savings.CashValue_ME_EX2, with lapses that
follow the fund applied to the monthly rate), in double precision, taking the
same inputs (a market path of 240 monthly returns for each scenario) and
giving the same outputs (four present values for the whole book on each
path). It holds every policy as a row and every path as a column and steps
all of them a month at a time.

It first checks itself against lifelib's answers for 200 kinds of policy on
1,000 paths, as the `liabilities` example does, then times the book.

    pip install "jax[cuda12]" numpy
    python savings_jax.py export [kinds of policy] [paths] [kinds at a time] [sorted]

where `export` is the folder liabilities_reference.py wrote to. With `sorted`
the policies are taken in order of the months they have left, and each lot is
stepped only as far as its longest policy runs, which is the obvious first
piece of tuning: without it every policy is stepped for all 241 months.
"""
import sys, time
import numpy as np
import jax
jax.config.update("jax_enable_x64", True)
import jax.numpy as jnp

out = sys.argv[1]
KINDS = int(sys.argv[2]) if len(sys.argv) > 2 else 20_000
PATHS = int(sys.argv[3]) if len(sys.argv) > 3 else 10_000
AT_A_TIME = int(sys.argv[4]) if len(sys.argv) > 4 else 2_500
SORTED = len(sys.argv) > 5 and sys.argv[5] == 'sorted'
MONTHS = 240
GROWTH, SWING = 0.02, 0.10

mortality = np.loadtxt(f'{out}/mort_table.csv', delimiter=',', skiprows=1)[:, 1:7]
surr_charge = np.loadtxt(f'{out}/surr_charge_table.csv', delimiter=',', skiprows=1)[:, 1:4]
disc_rate = np.loadtxt(f'{out}/disc_rate_ann.csv', delimiter=',', skiprows=1)[:, 1]
book = np.loadtxt(f'{out}/points_book.csv', delimiter=',', skiprows=1)

t = np.arange(MONTHS + 1)
discount = (1 + ((1 + disc_rate[t // 12]) ** (1 / 12) - 1)) ** (-t.astype(float))
inflation = 1.01 ** (t / 12)


def constants(rows):
    """What does not depend on the market path: for each policy and month,
    the chance of dying and of lapsing, the charge, the premium and so on."""
    level, charge_type, load = rows[:, 1] != 0, rows[:, 2].astype(int), rows[:, 3]
    age, term, count = rows[:, 4].astype(int), rows[:, 5].astype(int), rows[:, 6]
    promised, duration, premium, fund = rows[:, 7], rows[:, 8].astype(int), rows[:, 9], rows[:, 10]
    since = duration[None, :] + t[:, None]            # months since the policy began
    years = since // 12
    at_age = age[None, :] + years
    dies = np.where(at_age < len(mortality),
                    mortality[np.minimum(at_age, len(mortality) - 1), np.minimum(years, 5)], 0.0)
    dies = 1 - (1 - dies) ** (1 / 12)
    lapses = 1 - (1 - np.maximum(0.1 - 0.01 * years, 0.02)) ** (1 / 12)
    charge = np.where(charge_type[None, :] > 0,
                      surr_charge[np.minimum(years, 10), np.maximum(charge_type[None, :] - 1, 0)], 0.0)
    pays = np.where(level[None, :] | (since == 0), premium[None, :], 0.0)
    new = np.where(since == 0, count[None, :], 0.0)
    left = 12 * term - duration                       # months until the term ends
    return dict(dies=dies, lapses=lapses, charge=charge, pays=pays, new=new,
                paid_in=(1 - load)[None, :] * pays, left=left, promised=promised,
                policies=np.where(duration > 0, count, 0.0), fund=fund)


FEE = 0.01 / 12


def value(c, returns, fee):
    """Every policy of `c` on every path of `returns` (paths by months).
    Gives four present values for each policy and path, as four arrays."""
    promised = c['promised'][:, None]

    def month(state, x):
        fund, policies, net, on_death, at_end, fees = state
        k, dies, lapses, charge, pays, new, paid_in, growth = x
        running = (k < c['left'])[:, None]
        ends = (k == c['left'])[:, None]
        dies, lapses, charge, pays, new, paid_in = (
            a[:, None] for a in (dies, lapses, charge, pays, new, paid_in))
        before = policies + new
        with_premium = fund + paid_in
        invested = with_premium - fee * with_premium
        grew = growth[None, :] * invested
        mid = invested + 0.5 * grew
        died = before * dies
        follow = (1 - charge) * mid / promised
        lapsed = (before - died) * jnp.minimum(follow * lapses, 1.0)
        remain = before - lapsed - died
        next_fund = invested + grew
        death_claims = jnp.maximum(promised, mid) * died
        lapse_claims = mid * lapsed - charge * mid * lapsed
        premiums = pays * before
        expenses = 5000.0 * new + before * 500.0 / 12.0 * inflation_j[k]
        income = grew * remain + 0.5 * grew * (died + lapsed)
        fund_change = next_fund * remain - fund * policies
        flow = premiums + income - death_claims - lapse_claims - expenses - 0.05 * premiums - fund_change
        # The month the term ends: whoever is left is paid the fund, or the
        # promised amount if that is more.
        paid = jnp.maximum(promised, fund) * policies
        from_fund = fund * policies
        d = discount_j[k]
        net = net + jnp.where(running, flow, jnp.where(ends, from_fund - paid, 0.0)) * d
        on_death = on_death + jnp.where(running, death_claims - mid * died, 0.0) * d
        at_end = at_end + jnp.where(ends, paid - from_fund, 0.0) * d
        fees = fees + jnp.where(running, fee * with_premium * before, 0.0) * d
        fund = jnp.where(running, next_fund, fund)
        policies = jnp.where(running, remain, policies)
        return (fund, policies, net, on_death, at_end, fees), None

    shape = (c['fund'].shape[0], returns.shape[0])
    zero = jnp.zeros(shape)
    start = (jnp.broadcast_to(c['fund'][:, None], shape),
             jnp.broadcast_to(c['policies'][:, None], shape), zero, zero, zero, zero)
    # A last month with no growth, for policies whose term ends in month 240.
    growth = jnp.concatenate([returns.T, jnp.zeros((1, returns.shape[0]))])
    # Only as many months as the constants were worked out for.
    steps = c['dies'].shape[0]
    xs = (jnp.arange(steps), c['dies'], c['lapses'], c['charge'], c['pays'], c['new'],
          c['paid_in'], growth[:steps])
    (_, _, net, on_death, at_end, fees), _ = jax.lax.scan(month, start, xs)
    return net, on_death, at_end, fees


# For each policy and path, and added up over the policies for each path.
each_policy = jax.jit(lambda c, r: jnp.stack(value(c, r, FEE), axis=-1))
totals = jax.jit(lambda c, r: jnp.stack([a.sum(axis=0) for a in value(c, r, FEE)], axis=-1))


discount_j, inflation_j = jnp.asarray(discount), jnp.asarray(inflation)


def on_device(c):
    return {k: jnp.asarray(v) for k, v in c.items()}


# ---- Is it right? The same check as the liabilities example makes. ----
sample = on_device(constants(book[:200]))
paths = np.fromfile(f'{out}/returns_1000.f64').reshape(1000, 242)[:, :MONTHS]
theirs = np.fromfile(f'{out}/book_dynamic.f64').reshape(200, 1000, 4)
ours = np.asarray(each_policy(sample, jnp.asarray(paths)))
# As a share of the policy's largest answer, as the liabilities example measures it.
size = np.maximum(np.abs(theirs).max(axis=(1, 2)), 1.0)[:, None, None]
print(f'against lifelib, 200 kinds of policy on 1,000 paths: largest difference {(np.abs(ours - theirs) / size).max():.1e}')

# ---- The book. ----
rng = np.random.default_rng(1)
draws = rng.standard_normal((PATHS, MONTHS))
returns = np.exp((GROWTH - 0.5 * SWING**2) / 12 + SWING * np.sqrt(1 / 12) * draws) - 1
rows = book[:KINDS]
if SORTED:
    rows = rows[np.argsort(12 * rows[:, 5] - rows[:, 8], kind='stable')]


def lot(rows):
    """A lot of policies on the device, with constants for as many months as
    its longest policy runs (or all of them, when not sorted)."""
    c = constants(rows)
    steps = int(c['left'].max()) + 1 if SORTED else MONTHS + 1
    return on_device({k: (v[:steps] if v.ndim == 2 else v) for k, v in c.items()})


def run(returns):
    """From the paths on the host to the four totals for each path on the host."""
    r = jnp.asarray(returns)
    total = jnp.zeros((returns.shape[0], 4))
    for chunk in chunks:
        total = total + totals(chunk, r)
    return np.asarray(total)


started = time.perf_counter()
chunks = [lot(rows[i:i + AT_A_TIME]) for i in range(0, len(rows), AT_A_TIME)]
prepared = time.perf_counter() - started
started = time.perf_counter()
first = run(returns)
first_took = time.perf_counter() - started
times = []
for _ in range(5):
    started = time.perf_counter()
    again = run(returns)
    times.append(time.perf_counter() - started)
assert np.array_equal(first, again)
months = int((12 * rows[:, 5] - rows[:, 8] + 1).sum())
print(f'{len(rows):,} kinds of policy ({int(rows[:, 6].sum()):,} policies) on {PATHS:,} paths, {AT_A_TIME:,} kinds at a time{", sorted by months left" if SORTED else ""}, on {jax.devices()[0]}')
print(f'  constants worked out and copied to the device in {prepared:.2f} s')
print(f'  first run, with compiling: {first_took:.2f} s')
print(f'  five more runs: fastest {min(times):.3f} s, median {sorted(times)[2]:.3f} s, slowest {max(times):.3f} s')
print(f'  {months * PATHS / min(times):,.0f} months of policy a second at the fastest')
print(f'  what the book is worth on average: {first[:, 0].mean():,.0f}')
