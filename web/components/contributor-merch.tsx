"use client";

import Image from "next/image";
import Link from "next/link";
import { useRef, useState, type FormEvent } from "react";
import { PageHeader } from "@/components/page-header";
import { getMerchCopy, MERCH_LANGUAGES } from "@/lib/content/contributor-merch";
import { contributorCost, CONTRIBUTOR_CATALOG_SOURCE, validateContributorIdentity, type IdentityError } from "@/lib/contributor-merch";
import { MerchCheckout } from "@/components/merch-storefront";
import { getStorefrontCopy } from "@/lib/content/merch-storefront";
import { useCountryLabel } from "./use-country-label";
import { MERCH_COUNTRIES, MERCH_SIZES } from "@/lib/merch/catalog";
import checkoutStyles from "./merch-storefront.module.css";
import styles from "./contributor-merch.module.css";

/** Local print review/export followed by the shared, server-gated address-bound checkout. */
export function ContributorMerch({ locale }: { locale: string }) {
  const [language, setLanguage] = useState(locale);
  const [phrase, setPhrase] = useState(1);
  const [github, setGithub] = useState("");
  const [contribution, setContribution] = useState("");
  const [destination, setDestination] = useState("CN");
  const [size, setSize] = useState("M");
  const [error, setError] = useState<IdentityError | null>(null);
  const [draft, setDraft] = useState<Record<string, unknown> | null>(null);
  const githubInput = useRef<HTMLInputElement>(null);
  const contributionInput = useRef<HTMLInputElement>(null);
  const d = getMerchCopy(language);
  const labelCountry = useCountryLabel(language);
  const flow = getStorefrontCopy(language);
  const cost = contributorCost();
  const format = (value: number) => new Intl.NumberFormat(language, { style: "currency", currency: "CNY" }).format(value);
  const reset = () => { setDraft(null); setError(null); };
  function review(event: FormEvent) {
    event.preventDefault();
    const failure = validateContributorIdentity({ github, contribution });
    setError(failure);
    if (failure) {
      setDraft(null);
      (failure === "badUrl" ? contributionInput : githubInput).current?.focus();
      return;
    }
    setDraft({
      status: "local-draft-not-submitted",
      githubIdentity: github.trim().replace(/^@/, "") || null,
      contributionUrl: contribution.trim() || null,
      language, printPhrase: d.phrases[phrase], brand: "Codewhale", size,
      destination, product: "Yoycol DJCTX cotton DTF tee",
      costEstimateCny: destination === "CN" ? cost : null,
      source: CONTRIBUTOR_CATALOG_SOURCE,
      unverified: ["final artwork quote", "size availability", "delivery address quote", "tax", "actual FX", "contribution identity"],
    });
  }
  function download() {
    if (!draft) return;
    const url = URL.createObjectURL(new Blob([JSON.stringify(draft, null, 2)], { type: "application/json" }));
    const a = document.createElement("a");
    a.href = url; a.download = "codewhale-contributor-draft.json"; a.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
  const amount = (value: number) => destination === "CN" ? format(value) : d.quote;
  return (
    <div lang={language} dir={language === "ar" ? "rtl" : "ltr"}>
      <PageHeader title={d.title} lede={d.lede} pose="write" />
      <div className={`page-body ${styles.body}`}>
        <Link className={`section-link ${checkoutStyles.back}`} href={`/${locale}/merch`}>{flow.returnToMerch}</Link>
        <p className={checkoutStyles.notice}>{flow.contributorFlowNote}</p>
        <div className={styles.grid}>
          <form className={styles.form} onSubmit={review} noValidate aria-label={d.title}>
            <p data-testid="merch-product"><strong>{d.product}</strong> <span dir="ltr">· DJCTX</span></p>
            <label htmlFor="merch-language">{d.language}</label>
            <select id="merch-language" value={language} onChange={(e) => { setLanguage(e.target.value); reset(); }}>
              {MERCH_LANGUAGES.map((l) => <option key={l.code} value={l.code} lang={l.code} dir={l.code === "ar" ? "rtl" : "ltr"}>{l.label}</option>)}
            </select>
            <label htmlFor="merch-phrase">{d.phrase}</label>
            <select id="merch-phrase" value={phrase} onChange={(e) => { setPhrase(Number(e.target.value)); reset(); }}>
              {d.phrases.map((text, i) => <option key={i} value={i} dir="auto">{text}</option>)}
            </select>
            <label htmlFor="merch-github">{d.identity}</label>
            <input id="merch-github" name="github" ref={githubInput} value={github} maxLength={50} placeholder="@octocat / 1234567" autoComplete="off" autoCapitalize="none" spellCheck={false} dir="ltr" aria-invalid={error === "missing" || error === "badIdentity"} aria-describedby={error ? "merch-error" : undefined} onChange={(e) => { setGithub(e.target.value); reset(); }} />
            <label htmlFor="merch-contribution">{d.url}</label>
            <input id="merch-contribution" name="contribution" ref={contributionInput} value={contribution} maxLength={2048} placeholder="https://github.com/codewhale-hq/CodeWhale/pull/…" autoComplete="off" autoCapitalize="none" spellCheck={false} dir="ltr" aria-invalid={error === "badUrl"} aria-describedby={error ? "merch-error" : undefined} onChange={(e) => { setContribution(e.target.value); reset(); }} />
            <div className={styles.pair}>
              <div><label htmlFor="merch-destination">{d.destination}</label><select id="merch-destination" value={destination} onChange={(e) => { setDestination(e.target.value); reset(); }}>{MERCH_COUNTRIES.map((c) => <option key={c.code} value={c.code}>{labelCountry(c.code, c.name)}</option>)}</select></div>
              <div><label htmlFor="merch-size">{d.size}</label><select id="merch-size" value={size} onChange={(e) => { setSize(e.target.value); reset(); }}>{MERCH_SIZES.map((v) => <option key={v}>{v}</option>)}</select></div>
            </div>
            {error ? <p role="alert" id="merch-error" className={styles.error}>{d[error]}</p> : null}
            <button type="submit" className="btn btn-primary">{d.review}</button>
          </form>
          <aside className={styles.aside} aria-label={d.preview}>
            <div className={styles.print}>
              <span className={styles.caption}>{d.preview}</span>
              <Image src="/brand/mark-mono.svg" alt="" width={56} height={56} unoptimized />
              <p className={styles.printPhrase} dir="auto">{d.phrases[phrase]}</p>
              {d.phrases[phrase].includes("Codewhale") ? null : <span className={styles.brand}>Codewhale</span>}
              <span className={styles.site} dir="ltr">codewhale.net</span>
            </div>
            <section className={styles.cost} aria-labelledby="merch-cost">
              <h2 id="merch-cost">{d.costTitle}</h2>
              <p><strong>{d.product}</strong> <span dir="ltr">· DJCTX</span></p>
              <p>{d.costNote}</p>
              <dl>
                <div><dt>{d.production}</dt><dd>{amount(cost.production)}</dd></div>
                <div><dt>{d.processing}</dt><dd>{amount(cost.processing)}</dd></div>
                <div><dt>{d.shipping}</dt><dd>{amount(cost.shipping)}</dd></div>
                <div className={styles.total}><dt>{d.total}</dt><dd data-testid="merch-total">{amount(cost.total)}</dd></div>
              </dl>
              <p className={styles.note} dir="ltr">Alipay 3.9% + US$0.30 · 6.70 CNY/USD</p>
              <p className={styles.note}>{d.note}</p>
              <a href={CONTRIBUTOR_CATALOG_SOURCE} className="section-link" target="_blank" rel="noopener noreferrer">{d.source}</a>
            </section>
          </aside>
        </div>
        {draft ? <section className={styles.draft} role="status" aria-labelledby="merch-draft">
          <h2 id="merch-draft">{d.draft}</h2><p>{d.local}</p>
          <dl><div><dt>{d.identity}</dt><dd dir="auto">{github.trim() || "—"}</dd></div><div><dt>{d.url}</dt><dd dir="auto">{contribution.trim() || "—"}</dd></div><div><dt>{d.phrase}</dt><dd>{d.phrases[phrase]}</dd></div><div><dt>{d.total}</dt><dd>{amount(cost.total)}</dd></div></dl>
          <div className="actions"><button type="button" className="btn btn-primary" onClick={download}>{d.download}</button><button type="button" className="btn btn-secondary" onClick={() => { setDraft(null); githubInput.current?.focus(); }}>{d.edit}</button></div>
        </section> : null}
        {draft ? <section className={styles.draft} aria-labelledby="contributor-delivery-title"><h2 id="contributor-delivery-title">{flow.deliveryTitle}</h2><MerchCheckout locale={language} productId="contributor" country={destination} size={size} hideSelection contributor={{ github: github.trim().replace(/^@/, ""), contribution: contribution.trim(), language, phraseIndex: phrase }} /></section> : null}
      </div>
    </div>
  );
}
