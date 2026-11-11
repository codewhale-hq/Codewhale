"use client";

import Link from "next/link";
import { useEffect, useId, useRef, useState, type FormEvent } from "react";
import { getInterestCopy, getInterestItemCopy } from "@/lib/content/merch-interest";
import { MERCH_INTEREST_ITEMS, type MerchInterestCurrency, type MerchInterestItemId, type MerchInterestPriceChoice } from "@/lib/merch/interest-options";
import { MERCH_COUNTRIES } from "@/lib/merch/catalog";
import { useCountryLabel } from "./use-country-label";
import styles from "./merch-interest.module.css";

export function MerchInterest({ locale }: { locale: string }) {
  const d = getInterestCopy(locale);
  const id = useId();
  const [currency, setCurrency] = useState<MerchInterestCurrency>("cny");
  const [picks, setPicks] = useState<Partial<Record<MerchInterestItemId, MerchInterestPriceChoice>>>({});
  const [available, setAvailable] = useState<boolean | null>(null);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState("");
  const [email, setEmail] = useState("");
  const [country, setCountry] = useState("");
  const [consent, setConsent] = useState(false);
  const errorRef = useRef<HTMLParagraphElement>(null);
  const groups = ["cotton", "stretch", "desk", "plush"] as const;
  const labelCountry = useCountryLabel(locale);
  const prices = new Intl.NumberFormat(locale, { style: "currency", currency: currency.toUpperCase(), maximumFractionDigits: 0 });
  const changeCurrency = (value: MerchInterestCurrency) => {
    setCurrency(value);
    // A USD response must never inherit the visitor's CNY research selection.
    setPicks(Object.fromEntries(Object.keys(picks).map(key => [key, "unsure"])));
    setError("");
  };
  useEffect(() => {
    const controller = new AbortController();
    fetch("/api/merch/interest", { cache: "no-store", signal: controller.signal }).then(async response => {
      if (!response.ok) throw new Error("status");
      const value: unknown = await response.json();
      if (!value || typeof value !== "object" || !("available" in value) || typeof value.available !== "boolean") throw new Error("status");
      setAvailable(value.available);
    }).catch(() => { if (!controller.signal.aborted) setAvailable(false); });
    return () => controller.abort();
  }, []);
  useEffect(() => { if (error) errorRef.current?.focus(); }, [error]);

  async function join(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (saving || available !== true) return;
    if (!Object.keys(picks).length) { setError(d.needPick); return; }
    const form = new FormData(event.currentTarget);
    setSaving(true); setError("");
    try {
      const response = await fetch("/api/merch/interest", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({
        email: form.get("email"), country: form.get("country"), currency, locale,
        consent: form.get("consent") === "on", website: form.get("website"),
        picks: Object.entries(picks).map(([itemId, priceChoice]) => ({ id: itemId, priceChoice })),
      }) });
      const value: unknown = await response.json();
      if (!response.ok) { setError(response.status === 429 ? d.retry : response.status === 503 ? d.unavailable : d.error); return; }
      if (!value || typeof value !== "object" || !("ok" in value) || value.ok !== true) throw new Error("save");
      setSaved(true);
    } catch { setError(d.error); }
    finally { setSaving(false); }
  }

  return <section id="merch-interest" className={styles.interest} aria-labelledby={`${id}-title`}>
    <div className={styles.intro}><h2 id={`${id}-title`}>{d.title}</h2><p>{d.body}</p><p className={styles.note} id={`${id}-price-note`}>{d.priceNote}</p></div>
    {saved ? <div className={styles.confirmation} role="status" aria-live="polite"><h3>{d.saved}</h3><p>{d.savedNote}</p><button type="button" className="btn btn-secondary" onClick={() => setSaved(false)}>{d.edit}</button></div> : <form onSubmit={join} aria-describedby={`${id}-price-note`}>
      <fieldset disabled={saving} className={styles.formBody}>
        <div className={styles.currency}><label htmlFor={`${id}-currency`}>{d.currency}</label><select id={`${id}-currency`} value={currency} onChange={event => changeCurrency(event.target.value as MerchInterestCurrency)}><option value="cny">CNY · 人民币</option><option value="usd">USD · US dollar</option></select><p className={styles.note}>{d.currencyNote}</p></div>
        <fieldset className={styles.products}><legend>{d.products}</legend><p className={styles.note}>{d.pickNote}</p>
          {groups.map(group => <div className={styles.group} key={group}><h3>{d[group]}</h3>{group === "cotton" || group === "stretch" ? <p className={styles.note}>{d[group === "cotton" ? "cottonNote" : "stretchNote"]}</p> : null}
            <div>{MERCH_INTEREST_ITEMS.filter(item => item.group === group).map(item => { const itemCopy = getInterestItemCopy(item.id, locale); const selected = picks[item.id] !== undefined; return <div className={styles.item} key={item.id}>
              <label className={styles.choice}><input type="checkbox" checked={selected} onChange={event => { setPicks(current => { const next = { ...current }; if (event.target.checked) next[item.id] = "unsure"; else delete next[item.id]; return next; }); setError(""); }} /><span><strong>{itemCopy.name}</strong><span className={styles.detail}>{itemCopy.detail}</span></span></label>
              <div className={styles.budget}>{selected ? <><label htmlFor={`${id}-${item.id}-price`}>{d.price}<span className={styles.srOnly}> · {itemCopy.name}</span></label><select id={`${id}-${item.id}-price`} value={picks[item.id]} onChange={event => { setPicks(current => ({ ...current, [item.id]: event.target.value as MerchInterestPriceChoice })); setError(""); }}><option value="unsure">{d.unsure}</option>{(["low", "mid", "high"] as const).map((key, index) => <option value={key} key={key}>{prices.format(item.prices[currency][index])}</option>)}<option value="none">{d.none}</option></select></> : <p className={styles.range}>{item.prices[currency].map(price => prices.format(price)).join(" / ")}</p>}</div>
            </div>; })}</div>
          </div>)}
        </fieldset>
        <div className={styles.contact}><div className={styles.field}><label htmlFor={`${id}-email`}>{d.email}</label><input id={`${id}-email`} name="email" type="email" autoComplete="email" required maxLength={254} value={email} onChange={event => setEmail(event.target.value)} /></div><div className={styles.field}><label htmlFor={`${id}-country`}>{d.country}</label><input id={`${id}-country`} name="country" autoComplete="country-name" required maxLength={80} list={`${id}-countries`} aria-describedby={`${id}-country-hint`} value={country} onChange={event => setCountry(event.target.value)} /><datalist id={`${id}-countries`}>{MERCH_COUNTRIES.map(country => <option key={country.code} value={labelCountry(country.code, country.name)} />)}</datalist><p className={styles.note} id={`${id}-country-hint`}>{d.countryHint}</p></div></div>
        <div className={styles.trap} aria-hidden="true"><label htmlFor={`${id}-website`}>Website</label><input id={`${id}-website`} name="website" type="text" tabIndex={-1} autoComplete="off" maxLength={200} /></div>
        <label className={styles.consent}><input name="consent" type="checkbox" required checked={consent} onChange={event => setConsent(event.target.checked)} /><span>{d.consent}</span></label>
        <p className={styles.note}>{d.privacy} <Link href={`/${locale}/legal/privacy`}>{d.privacyLink}</Link></p>
        {available !== true ? <p role="status" className={styles.notice}>{available === null ? d.checking : d.unavailable}</p> : null}
        <button className="btn btn-primary" type="submit" disabled={saving || available !== true}>{saving ? d.saving : d.join}</button>
      </fieldset>
      {error ? <p role="alert" ref={errorRef} tabIndex={-1} className={styles.error}>{error}</p> : null}
    </form>}
  </section>;
}
