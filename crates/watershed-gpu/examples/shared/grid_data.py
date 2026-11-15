"""Fetches real readings for the grid_balancing example: what each unit on
Great Britain's grid told the operator it would do, and what was metered,
half hour by half hour.

    python3 grid_data.py /path/to/data              # a week from 13 January 2025
    python3 grid_data.py /path/to/data 2025-01-13 7

It writes `readings.csv` there, one row for each unit in each half hour that
has both a notification and a metered volume:

    date,period,unit,party,kind,notified_mwh,metered_mwh

A week is about 690,000 rows from 670 requests, and takes a few minutes. Each
day is kept as it arrives, so a run that stops can be started again.

The data is Elexon's, from its Insights API (https://bmrs.elexon.co.uk), which
needs no key, and is used under the BMRS data licence
(https://www.elexon.co.uk/bsc/data/balancing-mechanism-reporting-agent/copyright-licence-bmrs-data/):
contains BMRS data © Elexon Limited copyright and database right 2025.

- `notified_mwh` is the unit's final physical notification (PN): the levels
  in MW it said it would run at, added up over the half hour.
- `metered_mwh` is its actual metered volume (B1610). Putting energy in is
  positive and taking it out is negative, in both.
- `party` is the unit's lead party, and `kind` is `wind` for a wind farm,
  `demand` for a supplier's unit and `other` for the rest.
"""
import csv
import datetime as dt
import json
import pathlib
import sys
import time
import urllib.error
import urllib.request

API = 'https://data.elexon.co.uk/bmrs/api/v1'


def fetch(path):
    request = urllib.request.Request(f'{API}/{path}', headers={'User-Agent': 'watershed-example'})
    for attempt in range(6):
        try:
            with urllib.request.urlopen(request, timeout=120) as response:
                return json.load(response)
        except (urllib.error.URLError, TimeoutError, json.JSONDecodeError):
            if attempt == 5:
                raise
            time.sleep(2 ** attempt)


def rows(answer):
    return answer if isinstance(answer, list) else answer['data']


def when(text):
    return dt.datetime.fromisoformat(text.replace('Z', '+00:00'))


def kind(unit):
    if unit.get('fuelType') == 'WIND':
        return 'wind'
    # G is a supplier's unit for a region and S is another unit of a supplier's.
    return 'demand' if unit.get('bmUnitType') in ('G', 'S') else 'other'


def half_hour(units, date, period):
    """The rows for one half hour: units with a notification covering all of
    it, a metered volume and a lead party."""
    notified, covered = {}, {}
    for r in rows(fetch(f'datasets/PN?settlementDate={date}&settlementPeriod={period}&format=json')):
        unit = r['bmUnit']
        hours = (when(r['timeTo']) - when(r['timeFrom'])).total_seconds() / 3600
        notified[unit] = notified.get(unit, 0.0) + (r['levelFrom'] + r['levelTo']) / 2 * hours
        covered[unit] = covered.get(unit, 0.0) + hours
    out = []
    for r in rows(fetch(f'datasets/B1610?settlementDate={date}&settlementPeriod={period}&format=json')):
        unit = r['bmUnit']
        known = units.get(unit)
        if known and abs(covered.get(unit, 0.0) - 0.5) < 1e-6:
            out.append([date, period, unit, known['leadPartyId'], kind(known),
                        f'{notified[unit]:.6f}', f'{r["quantity"]:.6f}'])
    return sorted(out, key=lambda row: row[2])


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    folder = pathlib.Path(sys.argv[1])
    first = dt.date.fromisoformat(sys.argv[2]) if len(sys.argv) > 2 else dt.date(2025, 1, 13)
    days = int(sys.argv[3]) if len(sys.argv) > 3 else 7
    (folder / 'days').mkdir(parents=True, exist_ok=True)
    units = {u['elexonBmUnit']: u for u in rows(fetch('reference/bmunits/all')) if u.get('leadPartyId')}
    dates = [(first + dt.timedelta(days=d)).isoformat() for d in range(days)]
    for date in dates:
        kept = folder / 'days' / f'{date}.csv'
        if kept.exists():
            continue
        # A day the clocks change has 46 or 50 half hours; the rest have 48.
        day = []
        for period in range(1, 51):
            try:
                day += half_hour(units, date, period)
            except urllib.error.HTTPError as refused:
                if period <= 46 or refused.code != 400:
                    raise
        part = kept.with_suffix('.part')
        with part.open('w', newline='') as f:
            csv.writer(f).writerows(day)
        part.rename(kept)
        print(f'{date}: {len(day)} readings', flush=True)
    with (folder / 'readings.csv').open('w', newline='') as f:
        f.write('date,period,unit,party,kind,notified_mwh,metered_mwh\n')
        for date in dates:
            f.write((folder / 'days' / f'{date}.csv').read_text())
    total = sum(1 for _ in (folder / 'readings.csv').open()) - 1
    print(f'{total} readings in {folder / "readings.csv"}')


if __name__ == '__main__':
    main()
