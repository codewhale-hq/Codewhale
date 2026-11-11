"use client";

import Image from "next/image";
import Link from "next/link";
import { useEffect, useId, useRef, useState, type FormEvent } from "react";
import { getStorefrontCopy } from "@/lib/content/merch-storefront";
import { useCountryLabel } from "./use-country-label";
import { getMerchCopy } from "@/lib/content/contributor-merch";
import { MERCH_PRODUCTS, MERCH_COUNTRIES, MERCH_SIZES, type MerchProductId } from "@/lib/merch/catalog";
import type { Address, Quote as StoredQuote, QuoteInput } from "@/lib/merch/types";
import styles from "./merch-storefront.module.css";
import { MerchSizeGuide } from "./merch-size-guide";
import { MerchInterest } from "./merch-interest";
import { getInterestCopy } from "@/lib/content/merch-interest";

type Contributor = NonNullable<QuoteInput["contributor"]>;
type Quote = Pick<StoredQuote, "quoteId" | "expiresAt" | "currency" | "merchandise" | "shipping" | "processing" | "total" | "shippingName" | "taxNote">;
type Status = { checkoutConfigured: boolean; readyProductIds: string[] };
const emptyAddress: Address = { name: "", line1: "", line2: "", city: "", region: "", postalCode: "", phone: "" };

/** Shared address-bound checkout. No vendor order, fake stock or payment is created by this UI. */
export function MerchCheckout({ locale, productId, country, size, contributor, hideSelection = false }: {
  locale: string; productId: MerchProductId; country?: string; size?: string; contributor?: Contributor; hideSelection?: boolean;
}) {
  const d = getStorefrontCopy(locale);
  const id = useId();
  const [ownCountry, setCountry] = useState("CN");
  const [ownSize, setSize] = useState("XL");
  const [address, setAddress] = useState<Address>(emptyAddress);
  const [status, setStatus] = useState<Status | null>(null);
  const [statusFailed, setStatusFailed] = useState(false);
  const [busy, setBusy] = useState<"quote" | "checkout" | null>(null);
  const [error, setError] = useState("");
  const [quoted, setQuoted] = useState<{ quote: Quote; signature: string } | null>(null);
  const paymentStarted = useRef(false);
  const destination = country ?? ownCountry;
  const selectedSize = size ?? ownSize;
  const labelCountry = useCountryLabel(locale);
  const payload = { productId, size: selectedSize, color: "White", quantity: 1, country: destination, address, ...(contributor ? { contributor } : {}) };
  const signature = JSON.stringify(payload);
  // Any edit, including contributor print text/identity, immediately invalidates the visible quote.
  const quote = quoted?.signature === signature ? quoted.quote : null;
  const ready = status?.checkoutConfigured === true && status.readyProductIds.includes(productId);
  const countryName = MERCH_COUNTRIES.find((c) => c.code === destination);
  const displayCountry = labelCountry(destination, countryName?.name ?? destination);
  const errorMessage = (code: unknown, fallback: string) => code === "quote_expired" ? d.expired : code === "checkout_unavailable" ? d.notReady : code === "route_unavailable" || code === "product_unavailable" ? d.routeUnavailable : code === "try_later" ? d.tryLater : fallback;
  useEffect(() => {
    const controller = new AbortController();
    fetch("/api/merch/status", { signal: controller.signal, cache: "no-store" }).then(async (response) => {
      if (!response.ok) throw new Error("status");
      const value = await response.json() as Status;
      if (typeof value.checkoutConfigured !== "boolean" || !Array.isArray(value.readyProductIds) || !value.readyProductIds.every((product) => typeof product === "string")) throw new Error("status");
      setStatus(value);
    }).catch(() => { if (!controller.signal.aborted) setStatusFailed(true); });
    return () => controller.abort();
  }, []);
  useEffect(() => {
    if (!quoted || quoted.signature !== signature) return;
    const timeout = setTimeout(() => { setQuoted(null); setError(d.expired); }, Math.max(0, quoted.quote.expiresAt - Date.now()));
    return () => clearTimeout(timeout);
  }, [quoted, signature, d.expired]);
  async function review(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!ready || busy) return;
    setBusy("quote"); setError(""); setQuoted(null);
    try {
      const response = await fetch("/api/merch/quote", { method: "POST", headers: { "Content-Type": "application/json" }, body: signature });
      const value = await response.json() as Quote & { code?: string };
      if (!response.ok) { setError(errorMessage(value.code, d.quoteError)); return; }
      if (typeof value.quoteId !== "string" || !value.quoteId || !Number.isSafeInteger(value.expiresAt) || value.expiresAt <= Date.now() || !["cny", "usd"].includes(value.currency) || typeof value.shippingName !== "string" || typeof value.taxNote !== "string" || ![value.merchandise, value.shipping, value.processing, value.total].every((v) => Number.isSafeInteger(v) && v >= 0) || value.total !== value.merchandise + value.shipping + value.processing) throw new Error(d.quoteError);
      setQuoted({ quote: value, signature });
    } catch { setError(d.quoteError); }
    finally { setBusy(null); }
  }
  async function checkout() {
    if (!quote || !ready || busy || paymentStarted.current) return;
    if (quote.expiresAt <= Date.now()) { setQuoted(null); setError(d.expired); return; }
    paymentStarted.current = true; setBusy("checkout"); setError("");
    try {
      const response = await fetch("/api/merch/checkout", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ quoteId: quote.quoteId }) });
      const value = await response.json() as { url?: string; code?: string };
      if (!response.ok) { paymentStarted.current = false; setBusy(null); setError(errorMessage(value.code, d.checkoutError)); return; }
      const url = new URL(value.url ?? "");
      if (url.protocol !== "https:" || url.hostname !== "checkout.stripe.com" || url.username || url.password) throw new Error(d.checkoutError);
      window.location.assign(url.href);
    } catch { paymentStarted.current = false; setBusy(null); setError(d.checkoutError); }
  }
  const money = (cents: number) => new Intl.NumberFormat(locale, { style: "currency", currency: quote?.currency ?? "USD" }).format(cents / 100);
  const addressLimits: Record<keyof Address, number> = { name: 100, line1: 150, line2: 150, city: 100, region: 100, postalCode: 24, phone: 40 };
  const field = (name: keyof Address, label: string, autoComplete: string, optional = false) => <div className={styles.field} key={name}>
    <label htmlFor={`${id}-${name}`}>{label}</label>
    <input id={`${id}-${name}`} name={name} value={address[name]} required={!optional} autoComplete={`shipping ${autoComplete}`} maxLength={addressLimits[name]} type={name === "phone" ? "tel" : "text"} onChange={(event) => { setAddress({ ...address, [name]: event.target.value }); setError(""); }} />
  </div>;
  return <div className={styles.checkout}>
    <p role="status" className={styles.notice}>{statusFailed ? d.statusError : !status ? d.checking : !ready ? d.notReady : d.deliveryBody}</p>
    <form onSubmit={review}>
      <fieldset disabled={busy !== null}>
        {!hideSelection ? <div className={styles.pair}>
          <div className={styles.field}><label htmlFor={`${id}-country`}>{d.destination}</label><select id={`${id}-country`} value={destination} onChange={(event) => { setCountry(event.target.value); setError(""); }}>{MERCH_COUNTRIES.map((c) => <option key={c.code} value={c.code}>{labelCountry(c.code, c.name)}</option>)}</select></div>
          <div className={styles.field}><label htmlFor={`${id}-size`}>{d.size}</label><select id={`${id}-size`} value={selectedSize} onChange={(event) => { setSize(event.target.value); setError(""); }}>{MERCH_SIZES.map((s) => <option key={s} value={s}>{s}</option>)}</select></div>
        </div> : <p><strong>{displayCountry}</strong> · {selectedSize}</p>}
        <p className={styles.note}>{d.white}</p>
        {field("name", d.name, "name")}
        {field("line1", d.line1, "address-line1")}
        {field("line2", d.line2, "address-line2", true)}
        <div className={styles.pair}>{field("city", d.city, "address-level2")}{field("region", d.region, "address-level1", destination !== "US" && destination !== "CA")}</div>
        <div className={styles.pair}>{field("postalCode", d.postalCode, "postal-code", destination !== "US" && destination !== "CA")}{field("phone", d.phone, "tel")}</div>
        <p className={styles.note}>{d.addressNote}</p>
        <button className="btn btn-primary" disabled={!ready || busy !== null} type="submit">{busy === "quote" ? d.reviewing : d.reviewDelivery}</button>
      </fieldset>
    </form>
    {error ? <p className={styles.error} role="alert">{error}</p> : null}
    {quote ? <section className={styles.quote} aria-label={d.quoteTitle} aria-live="polite">
      <h3>{d.quoteTitle}</h3><p>{displayCountry} · {selectedSize} · {address.city}, {address.postalCode}</p>
      <dl><div><dt>{d.merchandise}</dt><dd>{money(quote.merchandise)}</dd></div><div><dt>{d.shipping}<br /><span className={styles.note}>{quote.shippingName}</span></dt><dd>{money(quote.shipping)}</dd></div><div><dt>{d.processing}</dt><dd>{money(quote.processing)}</dd></div><div className={styles.total}><dt>{d.total}</dt><dd>{money(quote.total)}</dd></div></dl>
      <p className={styles.note}>{quote.taxNote}</p>
      <button className="btn btn-primary" type="button" disabled={busy !== null || !ready} onClick={checkout}>{busy === "checkout" ? d.opening : d.payment}</button>
    </section> : null}
  </div>;
}

export function MerchStorefront({ locale }: { locale: string }) {
  const d = getStorefrontCopy(locale);
  const interest = getInterestCopy(locale);
  const [selected, setSelected] = useState<MerchProductId>("signature");
  const contributorPrint = getMerchCopy(locale);
  const seasonal = MERCH_PRODUCTS.find((p) => p.collection === "seasonal")!;
  const deskGear = MERCH_PRODUCTS.filter((p) => ["deskmat", "mousepad", "stickers"].includes(p.id));
  const stageDive = MERCH_PRODUCTS.find((p) => p.id === "stage-dive");
  const plush = MERCH_PRODUCTS.find((p) => p.id === "plush")!;
  const product = MERCH_PRODUCTS.find((p) => p.id === selected)!;
  const coreIds: readonly MerchProductId[] = ["signature", "classic", "wordmark", "ocean-line", "use-pocket"];
  const communityIds: readonly MerchProductId[] = ["whale-bro", "compiling", "use-codewhale"];
  const details: Partial<Record<MerchProductId, { title: string; body: string }>> = {
    signature: { title: d.heroTitle, body: d.heroBody }, classic: { title: d.classicTitle, body: d.classicBody },
    wordmark: { title: d.wordmarkTitle, body: d.wordmarkBody }, "ocean-line": { title: d.oceanTitle, body: d.oceanBody },
    "use-pocket": { title: d.pocketTitle, body: d.pocketBody }, "whale-bro": { title: d.whaleBroTitle, body: d.whaleBroBody },
    compiling: { title: d.compilingTitle, body: d.compilingBody }, "use-codewhale": { title: d.useTitle, body: d.useBody },
  };
  const detail = (id: MerchProductId) => details[id] ?? { title: d.heroTitle, body: d.designNote };
  const launchStyles = [
    { title: d.signatureTee, layout: d.signatureLayout, colors: d.signatureColors, fabric: d.cottonDirection },
    { title: d.classicTee, layout: d.classicLayout, colors: d.classicColors, fabric: d.cottonDirection },
    { title: d.whaleBroTee, layout: d.whaleBroLayout, colors: d.whaleBroColors, fabric: d.cottonDirection },
    { title: d.tidePolo, layout: d.poloLayout, colors: d.poloColors, fabric: d.poloDirection },
    { title: d.signatureHoodie, layout: d.signatureLayout, colors: d.hoodieColors, fabric: d.hoodieDirection },
    { title: d.tideTee, layout: d.tideLayout, colors: d.tideColors, fabric: d.ombreDirection },
    { title: d.mascotTee, layout: d.mascotLayout, colors: d.mascotTeeColors, fabric: d.mascotCotton },
    { title: d.mascotHoodie, layout: d.mascotHoodieLayout, colors: d.mascotHoodieColors, fabric: d.mascotLightweight },
  ];
  function choose(id: MerchProductId) { setSelected(id); document.getElementById("merch-delivery")?.scrollIntoView({ behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth", block: "start" }); }
  return <div className={styles.store}>
    <section className={styles.hero}>
      <figure className={styles.heroMedia}><Image className={styles.heroArt} src="/merch/mixed-launch-lineup.png" alt={d.launchAlt} width={1536} height={1024} sizes="(max-width: 700px) 100vw, 60vw" priority /><figcaption className={styles.note}>{d.designNote}</figcaption></figure>
      <div className={styles.heroText}><h1>{d.title}</h1><p>{d.description}</p><div className={styles.actions}><a className="btn btn-primary" href="#merch-interest">{interest.join}</a><a className="btn btn-secondary" href="#merch-lineup">{d.launchLink}</a><a className="btn btn-secondary" href="#merch-mascot">{d.mascotLink}</a><a className="btn btn-secondary" href="#merch-plush">{d.plushLink}</a><a className="btn btn-secondary" href="#merch-sizing">{d.fitLink}</a></div><p className={styles.preparing}>{d.preparing}</p><h2>{d.launchTitle}</h2><p>{d.launchBody}</p><p className={styles.note}>{d.launchPriceNote}</p></div>
    </section>
    <section className={`${styles.section} ${styles.seasonal}`} id="merch-mascot" aria-labelledby="merch-mascot-title"><Image className={styles.art} src="/merch/mascot-hoodies.png" alt={d.mascotAlt} width={1224} height={1285} sizes="(max-width: 700px) 100vw, 60vw" /><div><h2 id="merch-mascot-title">{d.mascotTitle}</h2><p>{d.mascotBody}</p><p className={styles.note}>{d.designNote}</p></div></section>
    <section className={`${styles.section} ${styles.seasonal}`} id="merch-plush" aria-labelledby="merch-plush-title"><Image className={styles.art} src={plush.image} alt={d.plushAlt} width={1222} height={1287} sizes="(max-width: 700px) 100vw, 60vw" /><div><h2 id="merch-plush-title">{d.plushTitle}</h2><p>{d.plushBody}</p><p className={styles.preparing}>{d.plushStatus}</p><p className={styles.note}>{d.designNote}</p></div></section>
    <section className={styles.section} id="merch-lineup"><div className={styles.sectionHead}><h2>{d.lineupTitle}</h2><p>{d.lineupBody}</p></div>
      <div className={styles.tableScroll} role="region" aria-label={d.lineupTitle} tabIndex={0}><table className={styles.lineupTable}><caption>{d.launchTitle}</caption><thead><tr><th scope="col">{d.styleLabel}</th><th scope="col">{d.printLabel}</th><th scope="col">{d.colorLabel}</th><th scope="col">{d.fabricLabel}</th></tr></thead><tbody>{launchStyles.map((style) => <tr key={style.title}><th scope="row">{style.title}</th><td>{style.layout}</td><td>{style.colors}</td><td>{style.fabric}</td></tr>)}</tbody></table></div>
      <p className={styles.note}>{d.paletteNote}</p>
    </section>
    <MerchInterest locale={locale} />
    <div className={styles.section} id="merch-sizing"><MerchSizeGuide locale={locale} /></div>
    <details className={`${styles.section} ${styles.archive}`}><summary>{d.archiveTitle}</summary><p className={styles.note}>{d.archiveBody}</p>
    <section className={styles.section} id="merch-tees"><div className={styles.sectionHead}><h2>{d.collection}</h2><p>{d.collectionBody}</p></div>
      <div className={styles.designs}>{MERCH_PRODUCTS.filter((p) => coreIds.includes(p.id)).map((p) => { const copy = detail(p.id); return <article key={p.id} className={styles.design}><Image className={`${styles.art} ${styles.corePrint}`} src={p.image} alt={copy.title} width={1200} height={900} sizes="(max-width: 700px) 100vw, 50vw" /><h3>{copy.title}</h3><p>{copy.body}</p><button type="button" className="btn btn-secondary" aria-pressed={selected === p.id} onClick={() => choose(p.id)}>{selected === p.id ? d.selected : d.select}</button></article>; })}</div>
      <p className={styles.note}>{d.colorNote}</p>
    </section>
    <section className={styles.section}><div className={styles.sectionHead}><h2>{d.community}</h2><p>{d.communityBody}</p></div><div className={styles.designs}>{MERCH_PRODUCTS.filter((p) => communityIds.includes(p.id)).map((p) => { const copy = detail(p.id); return <article key={p.id} className={styles.design}><Image className={styles.art} src={p.image} alt={copy.title} width={1200} height={900} sizes="(max-width: 700px) 100vw, 50vw" /><h3>{copy.title}</h3><p>{copy.body}</p><button type="button" className="btn btn-secondary" aria-pressed={selected === p.id} onClick={() => choose(p.id)}>{selected === p.id ? d.selected : d.select}</button></article>; })}</div></section>
    <section className={`${styles.section} ${styles.delivery}`} id="merch-delivery" aria-labelledby="merch-delivery-title"><div className={styles.deliverySummary}><h2 id="merch-delivery-title">{d.deliveryTitle}</h2><p className={styles.selected}>{detail(selected).title}</p><Image className={styles.art} src={product.image} alt={detail(selected).title} width={1200} height={900} sizes="(max-width: 700px) 100vw, 35vw" /><p className={styles.price}>{d.targetPrice}</p><p className={styles.note}>{d.priceNote}</p></div><MerchCheckout locale={locale} productId={selected} /></section>
    </details>
    <section className={`${styles.section} ${styles.contributor}`}><div><h2>{d.contributorTitle}</h2><p>{d.contributorBody}</p><Link href={`/${locale}/merch/contributor`} className="btn btn-primary">{d.contributorLink}</Link><p className={styles.note}>{d.contributorArtNote}</p></div><figure className={styles.contributorPrint}><Image src="/brand/mark-mono.svg" alt="" width={56} height={56} unoptimized /><p dir="auto">{contributorPrint.phrases[1]}</p><span>Codewhale</span><span dir="ltr">codewhale.net</span><figcaption className={styles.note}>{contributorPrint.preview}</figcaption></figure></section>
    <section className={`${styles.section} ${styles.seasonal}`}><Image className={styles.art} src={seasonal.image} alt={seasonal.title} width={1536} height={1024} sizes="(max-width: 700px) 100vw, 60vw" /><div><h2>{d.seasonal}</h2><h3>{seasonal.title}</h3><p>{d.seasonalBody}</p><p className={styles.note}>{d.concept}</p></div></section>
    {deskGear.length ? <section className={styles.section} id="merch-desk"><div className={styles.sectionHead}><h2>{d.deskTitle}</h2><p>{d.deskBody}</p></div><div className={styles.designs}>{stageDive ? <article className={styles.design}><Image className={styles.art} src={stageDive.image} alt={d.stageAlt} width={1536} height={1024} sizes="(max-width: 700px) 100vw, 50vw" /><h3>{d.stageTitle}</h3><p>{d.stageBody}</p><p className={styles.note}>{d.concept}</p></article> : null}<article className={styles.design}><Image className={styles.art} src="/merch/cache-mousepad-art.png" alt={d.cacheAlt} width={1134} height={1387} sizes="(max-width: 700px) 100vw, 50vw" /><h3>{d.cacheTitle}</h3><ul className={styles.deskItems}>{deskGear.map((p) => <li key={p.id}>{String(p.id) === "deskmat" ? d.deskmat : String(p.id) === "mousepad" ? d.mousepad : d.stickers}</li>)}</ul><p className={styles.note}>{d.concept}</p></article></div></section> : null}
    <section className={styles.section}><div className={styles.sectionHead}><h2>{d.later}</h2><p>{d.laterBody}</p></div><div className={styles.laterGrid}>{MERCH_PRODUCTS.filter((p) => p.collection === "later" && p.id === "accessories").map((p) => <article key={p.id}><Image className={styles.art} src={p.image} alt={d.accessoriesTitle} width={1536} height={1024} sizes="(max-width: 700px) 100vw, 50vw" /><h3>{d.accessoriesTitle}</h3><p className={styles.note}>{d.concept}</p></article>)}</div></section>
  </div>;
}
