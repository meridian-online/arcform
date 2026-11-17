# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = ["splink>=4,<5", "duckdb>=2,<3", "pandas", "pydantic>=2"]
# ///
"""End-to-end tests for resolve.py: the name key, and a real Splink run on a small corpus.

The GLEIF golden copy writes some of the best-known US companies with a leading article
that SEC EDGAR leaves out: EDGAR's BOEING CO is GLEIF's THE BOEING COMPANY. Before 2.0.0
the name key kept that THE, so the exact-name comparison failed and none of these
companies got an LEI. The fixture below uses the names exactly as the two sources publish
them, together with the near-miss entities GLEIF also holds under each name (the
companies' own retirement trusts, a lapsed foreign registration, an Asian subsidiary), so
a test that passes has also chosen the right one of several plausible LEIs.

Run with `uv run operators/splink_resolve/test_splink_resolve.py`. It needs Splink and
DuckDB, which uv resolves from the block above; CI runs it in the job that installs uv.
"""
from __future__ import annotations

import contextlib
import importlib.util
import io
import sys
import tempfile
import unittest
from pathlib import Path

import duckdb

HERE = Path(__file__).resolve().parent


def _load():
    spec = importlib.util.spec_from_file_location("splink_resolve_under_test", HERE / "resolve.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


sr = _load()

# (cik, ticker, company_name, exchange) as published in SEC's company_tickers_exchange.
EDGAR = [
    (12927, "BA", "BOEING CO", "NYSE"),
    (12927, "BA-PA", "BOEING CO", "NYSE"),
    (21344, "KO", "COCA COLA CO", "NYSE"),
    (1744489, "DIS", "Walt Disney Co", "NYSE"),
    (320193, "AAPL", "Apple Inc.", "Nasdaq"),
]

# (lei, legal_name, jurisdiction, country, registration_status) as published by GLEIF.
GLEIF = [
    ("RVHJWBXLJ1RFUBSY1F30", "THE BOEING COMPANY", "US-DE", "US", "ISSUED"),
    ("1YUFC5ZDM2WIXF56CS74", "The Boeing Company Employee Retirement Plans Master Trust", "US-DE", "US", "ISSUED"),
    ("7ZE7Y847DN5MYFKTGY57", "The Boeing Company Employee Savings Plans Master Trust", "US-DE", "US", "ISSUED"),
    ("984500DF8DC0ACD13946", "Boeing Consulting GmbH", "DE", "DE", "ISSUED"),
    ("UWJKFUJFZ02DKWI3RY53", "THE COCA-COLA COMPANY", "US-DE", "US", "ISSUED"),
    ("8945007BLGJ517U74C54", "The Coca-Cola Company", "AU", "AU", "LAPSED"),
    ("57VG5X0E00X0QJU7CQ58", "COCA-COLA CONSOLIDATED, INC.", "US-DE", "US", "ISSUED"),
    ("549300Z7BL7FW8J5BL73", "The Coca-Cola Company Master Retirement Trust", "US-DE", "US", "ISSUED"),
    ("549300GZKULIZ0WOW665", "THE WALT DISNEY COMPANY", "US-DE", "US", "ISSUED"),
    ("254900FT4XJ2MNNHOV07", "THE WALT DISNEY COMPANY (SOUTHEAST ASIA) PTE. LIMITED", "SG", "SG", "ISSUED"),
    ("FDXTRUS4WVXWQIT4BI58", "THE WALT DISNEY COMPANY RETIREMENT PLAN MASTER TRUST", "US-DE", "US", "ISSUED"),
    ("HWUPKR0MPOU8FGXBT394", "Apple Inc.", "US-CA", "US", "ISSUED"),
]


key = sr.name_key


class NameKeyTest(unittest.TestCase):
    def assertSameKey(self, edgar: str, gleif: str) -> None:
        self.assertEqual(key(edgar), key(gleif), f"{edgar!r} vs {gleif!r}")

    def assertDifferentKey(self, a: str, b: str) -> None:
        self.assertNotEqual(key(a), key(b), f"{a!r} vs {b!r}")

    def test_a_leading_article_does_not_separate_the_two_sources(self) -> None:
        for edgar, gleif in [
            ("BOEING CO", "THE BOEING COMPANY"),
            ("COCA COLA CO", "THE COCA-COLA COMPANY"),
            ("Walt Disney Co", "THE WALT DISNEY COMPANY"),
            ("GOLDMAN SACHS GROUP INC", "THE GOLDMAN SACHS GROUP, INC."),
        ]:
            with self.subTest(edgar=edgar):
                self.assertSameKey(edgar, gleif)

    def test_the_inside_a_word_or_a_name_is_kept(self) -> None:
        self.assertEqual(key("BANK OF THE OZARKS"), "BANK OF THE OZARKS")
        self.assertEqual(key("THEMIS MEDICAL CORP"), "THEMIS MEDICAL")

    def test_a_name_with_nothing_left_has_no_key(self) -> None:
        for name in ["The", "", None, "  "]:
            with self.subTest(name=name):
                self.assertEqual(key(name), "")

    def test_edgar_state_and_listing_tags_are_dropped(self) -> None:
        self.assertSameKey("FEDERAL SIGNAL CORP /DE/", "Federal Signal Corporation")
        self.assertSameKey("CITIZENS FINANCIAL GROUP INC/RI", "CITIZENS FINANCIAL GROUP, INC.")
        self.assertSameKey("Glencore plc/ADR", "GLENCORE PLC")
        self.assertSameKey("COSTCO WHOLESALE CORP /NEW", "COSTCO WHOLESALE CORPORATION")

    def test_stacked_legal_forms_are_all_dropped(self) -> None:
        self.assertSameKey("PETROCHINA CO LTD", "PetroChina Company Limited")
        self.assertSameKey("TREX CO INC", "Trex Corp Inc.")

    def test_holdings_and_group_still_tell_a_parent_from_its_subsidiary(self) -> None:
        self.assertDifferentKey("SIRIUS XM HOLDINGS INC.", "SIRIUS XM INC.")
        self.assertDifferentKey("Warner Music Group Corp.", "WARNER MUSIC INC.")
        self.assertEqual(key("Cigna Group"), key("THE CIGNA GROUP"))

    def test_and_co_is_part_of_the_name(self) -> None:
        self.assertDifferentKey("MORGAN STANLEY", "MORGAN STANLEY & CO. LLC")
        self.assertSameKey("WELLS FARGO & COMPANY/MN", "WELLS FARGO & COMPANY")
        self.assertSameKey("BECTON DICKINSON & CO", "BECTON, DICKINSON AND COMPANY")

    def test_an_llc_is_not_its_namesake_corporation(self) -> None:
        self.assertDifferentKey("CABOT CORP", "CABOT CO, LLC")
        self.assertSameKey("Lazard Group LLC", "LAZARD GROUP L.L.C.")

    def test_a_partnership_is_not_its_namesake_corporation(self) -> None:
        self.assertDifferentKey("JBG SMITH Properties", "JBG SMITH PROPERTIES LP")
        self.assertDifferentKey("CubeSmart", "CUBESMART, L.P.")
        self.assertSameKey("PLAINS ALL AMERICAN PIPELINE LP", "PLAINS ALL AMERICAN PIPELINE, L.P.")

    def test_ampersand_plus_and_and_agree(self) -> None:
        self.assertSameKey("PACIFIC GAS & ELECTRIC Co", "PACIFIC GAS AND ELECTRIC COMPANY")
        self.assertSameKey("Travel & Leisure Co.", "TRAVEL + LEISURE CO.")

    def test_dotted_and_undotted_initials_agree(self) -> None:
        self.assertSameKey("TELEFONICA S A", "TELEFONICA SA")
        self.assertSameKey("CEMEX SAB DE CV", "CEMEX, S.A.B. DE C.V.")
        self.assertSameKey("BANK OF N T BUTTERFIELD", "BANK OF N.T. BUTTERFIELD")
        self.assertDifferentKey("KB Financial Group Inc.", "K & B FINANCIAL GROUP, INC.")

    def test_initials_that_open_a_name_are_not_joined(self) -> None:
        self.assertDifferentKey("GAP INC", "G.A.P.")
        self.assertDifferentKey("CTS CORP", "C.T.S.")
        self.assertSameKey("J M SMUCKER Co", "THE J. M. SMUCKER COMPANY")

    def test_accents_fold(self) -> None:
        self.assertSameKey("Awale Resources Ltd", "Awalé Resources Limited")

    def test_a_translation_beside_the_name_is_dropped(self) -> None:
        self.assertSameKey("AMTD Digital Inc.", "AMTD Digital Inc. 尚乘數字科技集團")

    def test_a_word_mixing_scripts_leaves_no_key(self) -> None:
        self.assertEqual(key("BPエナジージャパン株式会社"), "")
        self.assertEqual(key("株式会社ＩＨＩインフラスクエア"), "")

    def test_the_japanese_legal_form_is_a_legal_form(self) -> None:
        self.assertSameKey("KDDI CORP", "KDDI株式会社")
        self.assertSameKey("IHI Corporation/ADR", "株式会社ＩＨＩ")


class ResolveTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.tmp = tempfile.TemporaryDirectory()
        d = Path(cls.tmp.name)
        con = duckdb.connect()
        con.execute("CREATE TABLE e(cik BIGINT, ticker VARCHAR, company_name VARCHAR, exchange VARCHAR)")
        con.executemany("INSERT INTO e VALUES (?, ?, ?, ?)", EDGAR)
        con.execute(
            "CREATE TABLE g(lei VARCHAR, legal_name VARCHAR, jurisdiction VARCHAR, "
            "country VARCHAR, registration_status VARCHAR)"
        )
        con.executemany("INSERT INTO g VALUES (?, ?, ?, ?, ?)", GLEIF)
        con.execute(f"COPY e TO '{d / 'edgar.parquet'}' (FORMAT parquet)")
        con.execute(f"COPY g TO '{d / 'gleif.parquet'}' (FORMAT parquet)")
        out = d / "crosswalk.parquet"
        with contextlib.redirect_stdout(io.StringIO()):
            sr.run(str(d / "edgar.parquet"), str(d / "gleif.parquet"), str(out), 0)
        cls.rows = {
            r[0]: r[1:]
            for r in duckdb.execute(
                f"SELECT ticker, lei, legal_name, method, status FROM read_parquet('{out}')"
            ).fetchall()
        }

    @classmethod
    def tearDownClass(cls) -> None:
        cls.tmp.cleanup()

    def test_each_company_published_with_the_is_confirmed_to_its_own_lei(self) -> None:
        for ticker, lei, legal_name in [
            ("BA", "RVHJWBXLJ1RFUBSY1F30", "THE BOEING COMPANY"),
            ("BA-PA", "RVHJWBXLJ1RFUBSY1F30", "THE BOEING COMPANY"),
            ("KO", "UWJKFUJFZ02DKWI3RY53", "THE COCA-COLA COMPANY"),
            ("DIS", "549300GZKULIZ0WOW665", "THE WALT DISNEY COMPANY"),
        ]:
            with self.subTest(ticker=ticker):
                self.assertIn(ticker, self.rows, f"{ticker} has no edge")
                self.assertEqual(self.rows[ticker], (lei, legal_name, "exact_name", "confirmed"))

    def test_a_company_without_the_still_matches(self) -> None:
        self.assertEqual(self.rows["AAPL"], ("HWUPKR0MPOU8FGXBT394", "Apple Inc.", "exact_name", "confirmed"))


if __name__ == "__main__":
    unittest.main()
