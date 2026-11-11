export const FOUNDER_SWITCHES = {
  cloudInvitationsOpen: { value: false, decision: "CURRENT_DECISIONS §132.1" },
  plansPublished: { value: false, decision: "CURRENT_DECISIONS §0" },
  cloudComputersBeyondInvite: { value: false, decision: "CURRENT_DECISIONS §132.1" },
  namedVendorAgentsOverAcp: { value: false, decision: "CURRENT_DECISIONS §82 and §69" },
} as const;

export interface FounderSwitchValues {
  cloudInvitationsOpen: boolean;
  plansPublished: boolean;
  cloudComputersBeyondInvite: boolean;
  namedVendorAgentsOverAcp: boolean;
}

export const FOUNDER_SWITCH_VALUES: FounderSwitchValues = {
  cloudInvitationsOpen: FOUNDER_SWITCHES.cloudInvitationsOpen.value,
  plansPublished: FOUNDER_SWITCHES.plansPublished.value,
  cloudComputersBeyondInvite: FOUNDER_SWITCHES.cloudComputersBeyondInvite.value,
  namedVendorAgentsOverAcp: FOUNDER_SWITCHES.namedVendorAgentsOverAcp.value,
};
