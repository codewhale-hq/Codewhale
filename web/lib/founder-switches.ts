export const FOUNDER_SWITCHES = {
  cloudInvitationsOpen: { value: false, decision: "CURRENT_DECISIONS §132.1" },
  plansPublished: { value: false, decision: "CURRENT_DECISIONS §0" },
} as const;

export interface FounderSwitchValues {
  cloudInvitationsOpen: boolean;
  plansPublished: boolean;
}

export const FOUNDER_SWITCH_VALUES: FounderSwitchValues = {
  cloudInvitationsOpen: FOUNDER_SWITCHES.cloudInvitationsOpen.value,
  plansPublished: FOUNDER_SWITCHES.plansPublished.value,
};
