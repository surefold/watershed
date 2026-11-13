"""Times lifelib keeping a valuation up to date as policies change, for the
`amendments` example to be compared with.

The model is savings.CashValue_ME_EX2 on one market path, with every kind of
policy from the book that `liabilities_reference.py` wrote. It is valued
once; then one policy's guaranteed amount is raised, several times over, and
the book's value found again each time: by valuing the whole book again,
which is what the model does when its table of policies changes, and by
valuing only the changed policy and adjusting the total by hand.

    python amendments_reference.py export [kinds of policy]

where `export` is the folder liabilities_reference.py wrote to.
"""
import sys, time
import numpy as np, pandas as pd
import lifelib, modelx as mx, os

out = sys.argv[1]
n = int(sys.argv[2]) if len(sys.argv) > 2 else 20_000
if not os.path.exists('savings'):
    lifelib.create('savings', 'savings')
P = mx.read_model('savings/CashValue_ME_EX2').Projection

P.product_spec_table = pd.DataFrame({
    'premium_type': ['SINGLE', 'SINGLE', 'LEVEL', 'LEVEL'],
    'has_surr_charge': [False, True, False, True],
    'surr_charge_id': [np.nan, 'type_1', np.nan, 'type_3'],
    'load_prem_rate': [0.0, 0.0, 0.10, 0.05],
    'is_wl': [False, False, False, False]}, index=pd.Index(list('ABCD'), name='spec_id'))

# Lapses that follow the fund, applied to the monthly rate, as in
# liabilities_reference.py.
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

flat = pd.read_csv(f'{out}/points_book.csv', index_col=0).iloc[:n]
spec = {(0, 0): 'A', (0, 1): 'B', (1, 0): 'C', (1, 3): 'D'}
book = pd.DataFrame({
    'spec_id': [spec[(a, b)] for a, b in zip(flat.level_premium, flat.surr_charge_type)],
    'age_at_entry': flat.age_at_entry, 'sex': 'M', 'policy_term': flat.policy_term,
    'policy_count': flat.policy_count, 'sum_assured': flat.sum_assured,
    'duration_mth': flat.duration_mth, 'premium_pp': flat.premium_pp,
    'av_pp_init': flat.av_pp_init, 'accum_prem_init_pp': 0}, index=flat.index)

P.scen_size = 1
P.model_point_table = book
np.asarray(P[1].inv_return_table(), dtype=np.float64).reshape(242).tofile(f'{out}/returns_1.f64')

def value():
    return float(np.sum(P[5].pv_net_cf()))

started = time.perf_counter()
before = value()
whole = time.perf_counter() - started

# One policy's guarantee goes up by a tenth: the whole book again.
changed, again, totals = [], [], []
for k in (7, 1234, 5000, 9999, 15000):
    k = k % n
    amended = book.copy()
    amended.iloc[k, amended.columns.get_loc('sum_assured')] *= 1.1
    started = time.perf_counter()
    P.model_point_table = amended
    totals.append(value())
    again.append(time.perf_counter() - started)
    changed.append(k)

# The same, valuing only the changed policy: its new value in place of its old.
alone = []
for k in changed:
    amended = book.iloc[[k]].copy()
    amended.iloc[0, amended.columns.get_loc('sum_assured')] *= 1.1
    started = time.perf_counter()
    P.model_point_table = amended
    value()
    alone.append(time.perf_counter() - started)

with open(f'{out}/amendments.csv', 'w') as f:
    f.write('kinds,whole_seconds,again_seconds,alone_seconds,before,first_changed,after\n')
    f.write(f'{n},{whole:.4f},{np.median(again):.4f},{np.median(alone):.4f},{before!r},{changed[0]},{totals[0]!r}\n')
print(open(f'{out}/amendments.csv').read())
