"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { currentNavHref, type ChromeLink } from "@/lib/i18n/links";
import { Icon } from "./icon";
import { ArrowLabel } from "./arrow-label";

export function MobileMenu({
  links,
  github,
  signIn,
  installHref,
  installLabel,
  openLabel,
  closeLabel,
  navAria,
}: {
  links: ChromeLink[];
  github: { href: string; label: string; count: string | null; aria: string };
  signIn: ChromeLink;
  installHref: string;
  installLabel: string;
  openLabel: string;
  closeLabel: string;
  /** Accessible name for the dialog's navigation landmark. */
  navAria: string;
}) {
  const [open, setOpen] = useState(false);
  // `closing` holds the panel mounted for a short exit fade; the unmount —
  // and the focus hand-back in the effect below — then happens on the
  // timeout, not on the click.
  const [closing, setClosing] = useState(false);
  const closeTimer = useRef<number | null>(null);
  const pathname = usePathname();
  const currentHref = currentNavHref([...links, signIn], pathname);
  const toggleRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  const closeImmediately = useCallback(() => {
    if (closeTimer.current !== null) window.clearTimeout(closeTimer.current);
    closeTimer.current = null;
    setOpen(false);
    setClosing(false);
  }, []);

  const close = useCallback(() => {
    // Reduced motion keeps the original instant mount/unmount.
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      closeImmediately();
      return;
    }
    if (closeTimer.current !== null) window.clearTimeout(closeTimer.current);
    setClosing(true);
    closeTimer.current = window.setTimeout(() => {
      closeTimer.current = null;
      setOpen(false);
      setClosing(false);
    }, 180);
  }, [closeImmediately]);

  const onToggle = () => {
    if (!open) {
      setOpen(true);
      return;
    }
    if (closing) {
      // Re-open mid-exit: cancel the pending unmount and stay open.
      window.clearTimeout(closeTimer.current ?? undefined);
      closeTimer.current = null;
      setClosing(false);
      return;
    }
    close();
  };

  useEffect(() => {
    if (!open) return;
    const prev = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    // aria-modal promises the dialog owns interaction. Keep background roots
    // inert, contain keyboard focus, and hand it back to the toggle on close.
    const dialog = menuRef.current;
    const backgroundRoots = Array.from(document.body.children)
      .filter((element): element is HTMLElement =>
        element instanceof HTMLElement && element !== dialog)
      .map((element) => ({ element, wasInert: element.inert }));
    for (const { element } of backgroundRoots) element.inert = true;

    // The toggle node is captured now: reading toggleRef.current inside the
    // cleanup would race React clearing the ref.
    const toggle = toggleRef.current;
    const focusable = () => Array.from(dialog?.querySelectorAll<HTMLElement>(
      'a[href], button:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])',
    ) ?? []).filter((element) => element.getClientRects().length > 0);
    focusable()[0]?.focus();

    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        close();
        return;
      }
      if (e.key !== "Tab") return;
      const candidates = focusable();
      const first = candidates[0];
      const last = candidates[candidates.length - 1];
      if (!first || !last) {
        e.preventDefault();
        return;
      }
      const active = document.activeElement;
      if (e.shiftKey && (active === first || !dialog?.contains(active))) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && (active === last || !dialog?.contains(active))) {
        e.preventDefault();
        first.focus();
      }
    };

    // Tailwind's lg boundary hides the compact controls. Close immediately
    // when a live resize crosses it so an invisible sheet cannot retain the
    // body's scroll lock.
    const desktop = window.matchMedia("(min-width: 1024px)");
    const onDesktop = (event: MediaQueryListEvent | MediaQueryList) => {
      if (event.matches) closeImmediately();
    };
    window.addEventListener("keydown", onKey);
    desktop.addEventListener("change", onDesktop);
    if (desktop.matches) closeImmediately();

    return () => {
      document.body.style.overflow = prev;
      window.removeEventListener("keydown", onKey);
      desktop.removeEventListener("change", onDesktop);
      for (const { element, wasInert } of backgroundRoots) element.inert = wasInert;
      if (toggle?.getClientRects().length) toggle.focus();
    };
  }, [close, closeImmediately, open]);

  // A pending exit timer must not outlive the component (locale switches
  // remount the nav).
  useEffect(() => {
    return () => {
      if (closeTimer.current !== null) window.clearTimeout(closeTimer.current);
    };
  }, []);

  return (
    <>
      <button
        ref={toggleRef}
        type="button"
        onClick={onToggle}
        className="nav-icon-button lg:hidden"
        aria-label={open ? closeLabel : openLabel}
        aria-expanded={open}
        aria-controls="mobile-menu"
      >
        <Icon name={open ? "x" : "menu"} className="nav-icon" />
      </button>

      {open && typeof document !== "undefined" &&
        createPortal(<div
          ref={menuRef}
          id="mobile-menu"
          className={`mm-panel lg:hidden fixed inset-0 z-40 bg-canvas overflow-y-auto${closing ? " mm-closing" : ""}`}
          role="dialog"
          aria-modal="true"
          aria-label={navAria}
        >
          <div className="flex min-h-[3.85rem] items-center justify-end px-4 hairline-b">
            <button type="button" onClick={close} className="nav-icon-button" aria-label={closeLabel}>
              <Icon name="x" className="nav-icon" />
            </button>
          </div>
          {/* Only one nav landmark is exposed at a time (the desktop nav is
              display:none at these widths), so the named dialog carries the
              landmark name and the inner nav stays unlabeled — two nested
              "Primary" landmarks would read as duplication. */}
          <nav className="px-6 py-4">
            <ul className="mm-list">
              {links.map((l) => {
                const isActive = l.href === currentHref;
                return (
                  <li key={l.href}>
                    <Link
                      href={l.href}
                      onClick={() => setOpen(false)}
                      className="mm-link"
                      aria-current={isActive ? "page" : undefined}
                    >
                      <span>{l.label}</span>
                      <Icon name="chevron-right" className="nav-icon mm-link-chevron icon-flip" />
                    </Link>
                  </li>
                );
              })}
              <li>
                <a
                  href={github.href}
                  className="mm-link"
                  aria-label={github.aria}
                  data-funnel="star_exit"
                  onClick={() => setOpen(false)}
                >
                  <span>{github.label}{github.count !== null && <span className="tabular"> · {github.count}</span>}</span>
                  <Icon name="chevron-right" className="nav-icon mm-link-chevron icon-flip" />
                </a>
              </li>
              <li>
                <Link
                  href={signIn.href}
                  onClick={() => setOpen(false)}
                  className="mm-link"
                  aria-current={signIn.href === currentHref ? "page" : undefined}
                >
                  <span>{signIn.label}</span>
                  <Icon name="chevron-right" className="nav-icon mm-link-chevron icon-flip" />
                </Link>
              </li>
            </ul>

            <Link
              href={installHref}
              onClick={() => setOpen(false)}
              className="mm-action paper-install-cta"
            >
              <ArrowLabel text={installLabel} />
            </Link>
          </nav>
        </div>, document.body)}
    </>
  );
}
