"""An independent working of the `dosing` example, to check it against.

Makes up some patients, doses each by the same protocol the example uses,
and follows the drug through the body with SciPy's adaptive solver for
differential equations (LSODA), which shares nothing with the example's way
of stepping. Writes the patients and three answers for each: the exposure
(area under the concentration curve) over the first day and over the third,
and the concentration at 72 hours.

    pip install numpy scipy
    python dosing_reference.py out_dir [patients]
"""
import sys
import numpy as np
from scipy.integrate import solve_ivp

out = sys.argv[1]
n = int(sys.argv[2]) if len(sys.argv) > 2 else 300
HOURS = 72

# The model: vancomycin in adults, two compartments (Goti et al., Ther Drug
# Monit 2018;40:212-221), without dialysis. These must match dosing.rs.
CL, CRCL_REF, CRCL_POWER, CRCL_MOST = 4.5, 120.0, 0.8, 150.0
V1, V2, Q = 58.4, 38.4, 6.5
# The protocol.
LOAD, LOAD_MOST, MAINT, MAINT_MOST = 25.0, 3000.0, 15.0, 2000.0
EVERY = [(90.0, 8), (50.0, 12), (20.0, 24), (0.0, 48)]  # kidney function at least: hours between doses
LONG_OVER = 1000.0  # a dose above this is given over two hours, not one

rng = np.random.default_rng(7)
age = rng.integers(18, 91, n).astype(float)
weight = np.round(np.clip(78 * np.exp(0.22 * rng.standard_normal(n)), 40, 180), 1)
female = (rng.random(n) < 0.45).astype(float)
creatinine = np.round(np.clip(0.9 * np.exp(0.45 * rng.standard_normal(n)), 0.4, 6.0), 2)
eta = rng.standard_normal((n, 3)) * np.sqrt(np.log(1 + np.array([0.398, 0.816, 0.571]) ** 2))

rows = []
for i in range(n):
    crcl = (140 - age[i]) * weight[i] / (72 * creatinine[i]) * (0.85 if female[i] else 1.0)
    crcl = min(crcl, CRCL_MOST)
    cl = CL * (crcl / CRCL_REF) ** CRCL_POWER * np.exp(eta[i, 0])
    v1 = V1 * weight[i] / 70 * np.exp(eta[i, 1])
    v2 = V2 * np.exp(eta[i, 2])
    every = next(hours for at_least, hours in EVERY if crcl >= at_least)
    load = min(LOAD * weight[i], LOAD_MOST)
    maint = min(MAINT * weight[i], MAINT_MOST)

    def rate(hour):
        """Milligrams an hour going in during this hour."""
        total = 0.0
        for start in [0] + list(range(every, HOURS, every)):
            dose = load if start == 0 else maint
            long = 2 if dose > LONG_OVER else 1
            if start <= hour < start + long:
                total += dose / long
        return total

    def flow(t, y, r):
        a1, a2, _ = y
        return [r - cl / v1 * a1 - Q / v1 * a1 + Q / v2 * a2, Q / v1 * a1 - Q / v2 * a2, a1 / v1]

    y, area = [0.0, 0.0, 0.0], {}
    for hour in range(HOURS):
        y = solve_ivp(flow, (hour, hour + 1), y, args=(rate(hour),), method='LSODA',
                      rtol=1e-11, atol=1e-11).y[:, -1]
        area[hour + 1] = y[2]
    rows.append([age[i], weight[i], female[i], creatinine[i], *eta[i],
                 area[24], area[72] - area[48], y[0] / v1])

np.savetxt(f'{out}/dosing_reference.csv', np.array(rows), delimiter=',', fmt='%.17g',
           header='age,weight,female,creatinine,eta_cl,eta_v1,eta_v2,area_day1,area_day3,at_72h', comments='')
print(f'{n} patients written to {out}/dosing_reference.csv')
