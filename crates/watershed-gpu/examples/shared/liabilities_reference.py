"""Runs the lifelib reference model (savings.CashValue_ME_EX2, MIT licence) and
writes what it was given and what it answered, for the watershed build to be
checked against."""
import os, sys, time
import numpy as np, pandas as pd
import lifelib, modelx as mx

OUT = 'export'
if not os.path.exists('savings'):
    lifelib.create('savings', 'savings')
m = mx.read_model('savings/CashValue_ME_EX2')
P = m.Projection

# The model's own tables.
P.mort_table.to_csv(f'{OUT}/mort_table.csv')
P.surr_charge_table.to_csv(f'{OUT}/surr_charge_table.csv')
P.disc_rate_ann.to_csv(f'{OUT}/disc_rate_ann.csv')

SPEC = {'A': (0, 0, 0.00), 'B': (0, 1, 0.00), 'C': (1, 0, 0.10), 'D': (1, 3, 0.05)}

def write_points(df, path):
    out = pd.DataFrame({
        'level_premium': [SPEC[s][0] for s in df.spec_id],
        'surr_charge_type': [SPEC[s][1] for s in df.spec_id],
        'load': [SPEC[s][2] for s in df.spec_id],
        'age_at_entry': df.age_at_entry, 'policy_term': df.policy_term,
        'policy_count': df.policy_count, 'sum_assured': df.sum_assured,
        'duration_mth': df.duration_mth, 'premium_pp': df.premium_pp,
        'av_pp_init': df.av_pp_init}, index=df.index)
    out.to_csv(path)

def run(name, points, scens, sim_id, patched=False):
    """One run of the reference; writes its four answers per (point, scenario)."""
    P.model_point_table = points
    P.scen_size = scens
    started = time.perf_counter()
    S = P[sim_id]
    cols = [S.pv_net_cf(), S.pv_claims_over_av('DEATH'), S.pv_claims_over_av('MATURITY'), S.pv_maint_fee()]
    took = time.perf_counter() - started
    np.column_stack([np.asarray(c, dtype=np.float64) for c in cols]).tofile(f'{OUT}/{name}.f64')
    months = int(S.max_proj_len())
    print(f'{name}: {len(points)} points x {scens} scenarios x {months} months, sim {sim_id}'
          f'{" (monthly dynamic lapse)" if patched else ""}: {took:.2f} s', flush=True)
    return took, months

def returns(scens):
    P.scen_size = scens
    t = P[1].inv_return_table()
    np.asarray(t, dtype=np.float64).reshape(scens, 242).tofile(f'{OUT}/returns_{scens}.f64')

# 1. The model as published: nine points, 10,000 scenarios, each of its
#    simulations without dynamic lapse.
published = P.model_point_moneyness.copy()
write_points(published, f'{OUT}/points_published.csv')
returns(10000)
timings = []
for sim in (1, 2, 3, 4):
    timings.append((f'published_{sim}', 9, 10000) + run(f'published_{sim}', published, 10000, sim))

# 2. A made-up book with every kind of product, to check the rest of the
#    rules: level premiums, surrender charges, policies part-way through.
P.product_spec_table = pd.DataFrame({
    'premium_type': ['SINGLE', 'SINGLE', 'LEVEL', 'LEVEL'],
    'has_surr_charge': [False, True, False, True],
    'surr_charge_id': [np.nan, 'type_1', np.nan, 'type_3'],
    'load_prem_rate': [0.0, 0.0, 0.10, 0.05],
    'is_wl': [False, False, False, False]}, index=pd.Index(list('ABCD'), name='spec_id'))

N = int(sys.argv[1]) if len(sys.argv) > 1 else 200_000
rng = np.random.default_rng(2026)
spec = rng.choice(list('ABCD'), N)
level = np.isin(spec, ['C', 'D'])
load = np.where(spec == 'C', 0.10, np.where(spec == 'D', 0.05, 0.0))
term = rng.choice([10, 15, 20], N)
age = rng.integers(20, 60, N)
count = rng.integers(1, 101, N)
sa = np.round(np.exp(rng.normal(np.log(300_000), 0.6, N)), -3)
dur = np.where(rng.random(N) < 0.05, 0, rng.integers(1, 12 * term))
single = np.round(sa * rng.uniform(0.9, 1.2, N), -2)
monthly = np.round(sa / (12 * term) * rng.uniform(1.0, 1.3, N))
prem = np.where(level, monthly, single)
av = np.where(level, monthly * (1 - load) * dur * rng.uniform(0.9, 1.2, N), sa * rng.uniform(0.9, 1.7, N))
av = np.where(dur == 0, 0.0, np.round(av))
book = pd.DataFrame({
    'spec_id': spec, 'age_at_entry': age, 'sex': 'M', 'policy_term': term, 'policy_count': count,
    'sum_assured': sa, 'duration_mth': dur, 'premium_pp': prem, 'av_pp_init': av,
    'accum_prem_init_pp': 0}, index=pd.RangeIndex(1, N + 1, name='point_id'))
write_points(book, f'{OUT}/points_book.csv')

K, S = 200, 1000
returns(S)
sample = book.iloc[:K]
timings.append(('book_static', K, S) + run('book_static', sample, S, 4))

# The same with lapses that follow how far the fund is above the guarantee,
# applied to the monthly rate (the model as published applies it to the
# yearly rate, which needs a power the watershed GPU path does not have yet).
def lapse_rate(t):
    if has_lapse():
        return np.maximum(0.1 - 0.01 * duration(t), 0.02)
    else:
        return pd.Series(0, index=model_point().index)

def pols_lapse(t):
    factor = csv_pp(t) / sum_assured() if is_lapse_dynamic() else 1
    monthly = factor * (1 - (1 - lapse_rate(t))**(1/12))
    return (pols_if_at(t, "BEF_DECR") - pols_death(t)) * np.minimum(monthly, 1)

P.lapse_rate.formula = lapse_rate
P.pols_lapse.formula = pols_lapse
timings.append(('book_dynamic', K, S) + run('book_dynamic', sample, S, 5, patched=True))

# How fast the reference goes: rows (point x scenario) x months a second.
with open(f'{OUT}/timings.csv', 'w') as f:
    f.write('run,points,scenarios,seconds,months\n')
    for name, points, scens, took, months in timings:
        f.write(f'{name},{points},{scens},{took:.3f},{months}\n')
