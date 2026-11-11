import { runtimeCapabilityContext, capabilityAvailability, type CapabilityContext } from "../capabilities";
import { FACTS } from "../facts.generated";
import { FEATURE_CATALOGUE, type Feature } from "../features";
import type { HomeDict } from "../i18n/dictionaries/types";
import { surfaceState, type SurfaceContext, type SurfaceId, type SurfaceState } from "../surface-states";

export const PRODUCT_MAP_IDS = ["terminal", "localBrowser", "manyAgents", "desktop", "cloud", "oneSession"] as const;
export type ProductMapId = (typeof PRODUCT_MAP_IDS)[number];

export const PRODUCT_MAP_STATES = ["released", "developing", "inviteOnly", "beingBuilt"] as const;
export type ProductMapState = (typeof PRODUCT_MAP_STATES)[number];

export const PRODUCT_MAP_STATE_KEY = {
  released: "mapStateReleased",
  developing: "mapStateDeveloping",
  inviteOnly: "mapStateInviteOnly",
  beingBuilt: "mapStateBeingBuilt",
} as const satisfies Record<ProductMapState, keyof HomeDict>;

export const PRODUCT_MAP_TEXT_KEYS = {
  terminal: { name: "mapTerminal", sentence: "mapTerminalSentence" },
  localBrowser: { name: "mapLocalBrowser", sentence: "mapLocalBrowserSentence" },
  manyAgents: { name: "mapManyAgents", sentence: "mapManyAgentsSentence" },
  desktop: { name: "mapDesktop", sentence: "mapDesktopSentence" },
  cloud: { name: "mapCloud", sentence: "mapCloudSentence" },
  oneSession: { name: "mapOneSession", sentence: "mapOneSessionSentence" },
} as const satisfies Record<ProductMapId, { name: keyof HomeDict; sentence: keyof HomeDict }>;

const SURFACE_OF: Record<Exclude<ProductMapId, "manyAgents" | "oneSession">, SurfaceId> = {
  terminal: "terminal",
  localBrowser: "local-browser",
  desktop: "desktop",
  cloud: "cloud-computers",
};

const SENTENCE_STATE: Record<ProductMapId, ProductMapState> = {
  terminal: "released",
  localBrowser: "released",
  manyAgents: "released",
  desktop: "developing",
  cloud: "inviteOnly",
  oneSession: "beingBuilt",
};

const MANY_AGENT_FEATURES = ["subagents", "fleet", "workflows"] as const;

export interface ProductMapInput {
  surface?: SurfaceContext;
  capabilities?: CapabilityContext;
  features?: readonly Feature[];
}

export interface ProductMapRow {
  id: ProductMapId;
  state: ProductMapState;
  sentenceHolds: boolean;
}

function fromSurface(state: SurfaceState): ProductMapState {
  switch (state) {
    case "released":
    case "available":
    case "open":
      return "released";
    case "invite_only":
      return "inviteOnly";
    case "not_open":
    case "not_public":
      return "beingBuilt";
    default:
      return "developing";
  }
}

function manyAgentsState(context: CapabilityContext, features: readonly Feature[]): ProductMapState {
  const rows = MANY_AGENT_FEATURES.map((id) => features.find((feature) => feature.id === id));
  if (rows.some((row) => !row)) return "beingBuilt";
  const availability = rows.map((row) => capabilityAvailability(row!, context));
  if (availability.every((value) => value === "released")) return "released";
  return availability.some((value) => value === "hidden") ? "beingBuilt" : "developing";
}

export function productMap(input: ProductMapInput = {}): ProductMapRow[] {
  const surface = input.surface ?? {};
  const capabilities = input.capabilities ?? runtimeCapabilityContext(FACTS);
  const features = input.features ?? FEATURE_CATALOGUE.features;
  return PRODUCT_MAP_IDS.map((id) => {
    const state: ProductMapState =
      id === "manyAgents"
        ? manyAgentsState(capabilities, features)
        : id === "oneSession"
          ? "beingBuilt"
          : fromSurface(surfaceState(SURFACE_OF[id], surface));
    return { id, state, sentenceHolds: state === SENTENCE_STATE[id] };
  });
}
