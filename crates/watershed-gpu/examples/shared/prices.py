"""Fetches daily share prices, as far back as 1970, for the `replay` example.

It takes today's members of the S&P 500 from Wikipedia's list and asks Yahoo
Finance for each one's daily history, which starts in 1970 at the earliest.
Each is written as `<TICKER>.csv` with the columns
`Date,Open,High,Low,Close,Volume`, the form the `backtest` and `experts`
examples also read. It ends by saying how many have prices from each of
several years on: when this was written, 26 from 1970, 44 from 1973, 93 from
1980 and 201 from 1985. The example chooses the year, and so the shares.

    python3 crates/watershed-gpu/examples/shared/prices.py prices

`Close` is Yahoo's adjusted close: the price with splits and dividends
taken back through its history, so that one day's close over the last is
what a holder made that day. Open, high and low are as Yahoo gives them,
adjusted for splits only.

Two things to know about what this gets:

- Only companies that are in the index today. Every company that failed or
  was bought on the way is missing, so any portfolio drawn from these did
  better than one chosen at the time could have. It is a set of survivors,
  and the further back the year, the fewer and the luckier they are.
- Yahoo's data is for personal use. The prices are fetched when this is
  run and are not part of the repository.

It needs nothing but Python's standard library. With `--index`, it also
fetches the S&P 500 index itself as `_GSPC.csv`.
"""

import csv
import datetime
import json
import os
import re
import sys
import time
import urllib.request

MEMBERS = "https://en.wikipedia.org/wiki/List_of_S%26P_500_companies"
HISTORY = ("https://query1.finance.yahoo.com/v8/finance/chart/{}"
           "?period1=0&period2=4000000000&interval=1d&events=div%2Csplit")
# A share is kept if its last price is within this many days of today.
STALE = 10


def fetch(url):
    request = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0"})
    for attempt in range(4):
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                return response.read()
        except Exception as error:  # noqa: BLE001  (a refusal or a timeout: wait and try again)
            if attempt == 3:
                raise
            time.sleep(2 ** attempt)
            last = error
    raise last


def members():
    """Today's members' tickers, from the first table of Wikipedia's list."""
    page = fetch(MEMBERS).decode("utf-8", "replace")
    table = page[page.index('id="constituents"'):]
    table = table[:table.index("</table>")]
    tickers = []
    for row in table.split("<tr")[2:]:
        cell = re.search(r"<td[^>]*>(.*?)</td>", row, re.S)
        if cell:
            ticker = re.sub(r"<[^>]+>", "", cell.group(1)).strip()
            if re.fullmatch(r"[A-Z.]{1,6}", ticker):
                tickers.append(ticker)
    return tickers


def history(ticker):
    """A ticker's days as (date, open, high, low, adjusted close, volume)."""
    # Yahoo writes a class of share with a dash: BRK.B is BRK-B.
    reply = json.loads(fetch(HISTORY.format(ticker.replace(".", "-"))))
    result = (reply.get("chart", {}).get("result") or [None])[0]
    if not result or not result.get("timestamp"):
        return []
    quote = result["indicators"]["quote"][0]
    adjusted = result["indicators"].get("adjclose", [{}])[0].get("adjclose") or quote["close"]
    days = []
    for k, stamp in enumerate(result["timestamp"]):
        day = datetime.datetime.fromtimestamp(stamp, datetime.timezone.utc).date()
        row = [quote[name][k] for name in ("open", "high", "low")] + [adjusted[k], quote["volume"][k]]
        if row[3] is not None and row[3] > 0:
            days.append([day] + [0 if x is None else x for x in row])
    return days


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    if len(args) != 1:
        raise SystemExit("give a folder to write the prices to")
    folder = args[0]
    os.makedirs(folder, exist_ok=True)
    tickers = members()
    if "--index" in sys.argv:
        tickers.append("^GSPC")
    today = datetime.date.today()
    kept, starts = 0, []
    for at, ticker in enumerate(tickers, start=1):
        try:
            days = history(ticker)
        except Exception as error:  # noqa: BLE001
            print(f"{ticker}: {error}", file=sys.stderr)
            continue
        if days and (today - days[-1][0]).days <= STALE:
            starts.append(days[0][0])
            with open(os.path.join(folder, ticker.replace("^", "_") + ".csv"), "w", newline="") as f:
                w = csv.writer(f)
                w.writerow(["Date", "Open", "High", "Low", "Close", "Volume"])
                for day in days:
                    w.writerow([day[0].isoformat()] + [f"{x:.6g}" for x in day[1:5]] + [int(day[5])])
            kept += 1
        if at % 50 == 0:
            print(f"{at} of {len(tickers)} asked for, {kept} kept", file=sys.stderr)
        time.sleep(0.15)
    print(f"{kept} of {len(tickers)} written to {folder}", file=sys.stderr)
    for year in (1970, 1973, 1975, 1980, 1985, 1990, 2000):
        since = sum(1 for start in starts if start <= datetime.date(year, 1, 15))
        print(f"  with prices from the start of {year}: {since}", file=sys.stderr)


if __name__ == "__main__":
    main()
