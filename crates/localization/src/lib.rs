//! Lightweight localization registry for high-visibility TUI strings.
//!
//! This intentionally covers UI chrome only. It does not change model prompts,
//! model output language, provider behavior, or media payload semantics.
use rust_i18n::i18n;
include!(concat!(env!("OUT_DIR"), "/i18n_init.rs"));
mod localization_backend;

use std::borrow::Cow;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Locale {
    En,
    Ja,
    ZhHans,
    ZhHant,
    PtBr,
    Es419,
    Vi,
    Ko,
    Ca,
    De,
    Fr,
    Id,
    Hi,
    Ru,
    Uk,
}

impl Locale {
    pub fn tag(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Ja => "ja",
            Self::ZhHans => "zh-Hans",
            Self::ZhHant => "zh-Hant",
            Self::PtBr => "pt-BR",
            Self::Es419 => "es-419",
            Self::Vi => "vi",
            Self::Ko => "ko",
            Self::Ca => "ca",
            Self::De => "de",
            Self::Fr => "fr",
            Self::Id => "id",
            Self::Hi => "hi",
            Self::Ru => "ru",
            Self::Uk => "uk",
        }
    }

    pub fn translation_target_name(self) -> &'static str {
        match self {
            Self::En => "English",
            Self::Ja => "Japanese (日本語)",
            Self::ZhHans => "Simplified Chinese (简体中文)",
            Self::ZhHant => "Traditional Chinese (繁體中文)",
            Self::PtBr => "Brazilian Portuguese (Português do Brasil)",
            Self::Es419 => "Latin American Spanish (Español latinoamericano)",
            Self::Vi => "Vietnamese (Tiếng Việt)",
            Self::Ko => "Korean (한국어)",
            Self::Ca => "Catalan (Català)",
            Self::De => "German (Deutsch)",
            Self::Fr => "French (Français)",
            Self::Id => "Indonesian (Bahasa Indonesia)",
            Self::Hi => "Hindi (हिन्दी)",
            Self::Ru => "Russian (Русский)",
            Self::Uk => "Ukrainian (Українська)",
        }
    }

    /// Every locale the TUI exposes in pickers and runtime resolution.
    pub fn shipped() -> &'static [Self] {
        &[
            Self::En,
            Self::Ja,
            Self::ZhHans,
            Self::ZhHant,
            Self::PtBr,
            Self::Es419,
            Self::Vi,
            Self::Ko,
            Self::Ca,
            Self::De,
            Self::Fr,
            Self::Id,
            Self::Hi,
            Self::Ru,
            Self::Uk,
        ]
    }

    /// Complete UI packs held to `en.json` parity.
    pub fn shipped_complete() -> &'static [Self] {
        &[
            Self::En,
            Self::Ja,
            Self::ZhHans,
            Self::ZhHant,
            Self::PtBr,
            Self::Es419,
            Self::Vi,
            Self::Ko,
            Self::Ca,
            Self::De,
            Self::Fr,
            Self::Id,
            Self::Hi,
            Self::Ru,
            Self::Uk,
        ]
    }

    #[must_use]
    pub fn is_partial_pack(self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageId {
    MobileStreamReplayFailed,
    MobileStreamCatchUpFailed,
    MobileStreamRuntimeShutdown,
    MobileStreamEnded,
    MobileStreamClosed,
    MobileStreamReconnecting,
    MobileStreamConnected,
    ConversationChangedBeforeUndo,
    SessionArchiveExported,
    SessionArchiveSizes,
    SessionArchiveNoArtifacts,
    SessionArchiveRestoreHint,
    CostReasonNotMoney,
    CostReasonBillingUnknown,
    CostReasonEndpointUnknown,
    CostReasonRateMissing,
    CostReasonLiveUnverified,
    CostReasonRetiredAlias,
    CostReasonTierMissing,
    CostReasonRoutingDependent,
    CostReasonCoverageMissing,
    CostReasonTokenRateMissing,
    CostReasonInvalidRate,
    CostReasonCurrencyMissing,
    CostReasonUsageConflict,
    CostChipUnknown,
    CostChipSubtotal,
    CostChipSavedSubtotal,
    CostChipLocal,
    CostChipAllowance,
    CostChipAllowancePercent,
    McpLoginInProgress,
    McpLoginStarting,
    McpLoginBrowser,
    McpLoginStored,
    McpLoginFailed,
    McpReloadAlreadyRunning,
    McpLoginCancelled,
    McpLoginHandshakeTimeout,
    McpLoginServerNotFound,
    McpDiagnosisUnobserved,
    McpDiagnosisSummary,
    McpDiagnosisLastError,
    McpDiagnosisNext,
    McpStateDisabled,
    McpStateAuthorizationRequired,
    McpStateConnecting,
    McpStateFailed,
    McpStateDisconnected,
    ComposerPlaceholder,
    ComposerDispatchFailedRestored,
    DispatchFailedQueued,
    DispatchFailedInitial,
    HistorySearchPlaceholder,
    HistorySearchTitle,
    HistoryHintMove,
    HistoryHintAccept,
    HistoryHintRestore,
    HistoryNoMatches,
    TranscriptReasoningExpand,
    TranscriptThought,
    TranscriptThoughtFor,
    ScreenModeFullscreenNotice,
    ScreenModeInlineNotice,
    ScreenModeMouseCaptureOn,
    ScreenModeMouseCaptureOff,
    ScreenModeUnchanged,
    ImageInputRejectedResent,
    ImageInputOmitted,
    ProviderToolCallMissing,
    // First-run anonymous usage disclosure.
    TelemetryNoticeDefaultOn,
    TelemetryNoticeHeadline,
    TelemetryNoticeBody,
    TelemetryNoticeCompactBody,
    TelemetryNoticeChoiceKeep,
    TelemetryNoticeChoiceDisable,
    TelemetryNoticeActionChoose,
    TelemetryNoticeActionConfirm,
    TelemetryNoticeActionExit,
    TelemetryNoticeReceiptEnabled,
    TelemetryNoticeReceiptDisabled,
    TelemetryNoticeReceiptEnabledUnsaved,
    TelemetryNoticeReceiptDisabledUnsaved,
    TelemetryPreferenceEnabledNextLaunch,
    TelemetryPreferenceDisabled,
    TelemetryPreferenceDisabledWithWarning,
    TelemetryPreferenceDisabledForSession,
    TelemetryPreferenceSaveFailed,
    // StatusPicker — `/statusline` multi-select footer-item picker.
    StatusPickerTitle,
    StatusPickerInstruction,
    StatusPickerActionToggle,
    StatusPickerActionAll,
    StatusPickerActionNone,
    StatusPickerActionSave,
    StatusPickerActionCancel,
    // Hotbar setup wizard chrome and validation.
    HotbarSetupTitle,
    HotbarSetupSourceApp,
    HotbarSetupSourceSlash,
    HotbarSetupSourceMcp,
    HotbarSetupSourceSkill,
    HotbarSetupSourcePlugin,
    HotbarSetupStatusDisabled,
    HotbarSetupStatusPrefill,
    HotbarSetupStatusReady,
    HotbarSetupDirtyModified,
    HotbarSetupDirtyClean,
    HotbarSetupNoAction,
    HotbarSetupConfirmDisable,
    HotbarSetupStatusLine,
    HotbarSetupSlotOutOfRange,
    HotbarSetupNoActionSelected,
    HotbarSetupCannotAssign,
    HotbarSetupNoActions,
    HotbarSetupRecommended,
    HotbarSetupEmptySlot,
    HotbarSetupHelp,
    HotbarActionVoiceToggleName,
    HotbarActionVoiceToggleDescription,
    HotbarActionSessionCompactName,
    HotbarActionSessionCompactDescription,
    HotbarActionModePlanName,
    HotbarActionModePlanDescription,
    HotbarActionModeAgentName,
    HotbarActionModeAgentDescription,
    HotbarActionModeYoloName,
    HotbarActionModeYoloDescription,
    HotbarActionModeOperateName,
    HotbarActionModeOperateDescription,
    HotbarActionReasoningCycleName,
    HotbarActionReasoningCycleDescription,
    HotbarActionReasoningCycleAutoDisabled,
    HotbarActionSidebarToggleName,
    HotbarActionSidebarToggleDescription,
    HotbarActionFileTreeToggleName,
    HotbarActionFileTreeToggleDescription,
    HotbarActionPaletteOpenName,
    HotbarActionPaletteOpenDescription,
    HotbarActionTrustToggleName,
    HotbarActionTrustToggleDescription,
    CommandPaletteTitle,
    CommandPaletteSubtitle,
    ConfigTitle,
    ConfigPreviewLabel,
    ConfigHintExternalCredentials,
    ConfigSubtitle,
    ConfigModalTitle,
    ConfigSearchPlaceholder,
    ConfigNoSettings,
    ConfigNoMatchesPrefix,
    ConfigFilteredSettings,
    ConfigShowing,
    ConfigFooterDefault,
    ConfigFooterScrollable,
    ConfigFooterFiltered,
    ConfigSectionProvider,
    ConfigSectionModel,
    ConfigSectionPermissions,
    ConfigSectionNetwork,
    ConfigSectionDisplay,
    ConfigSectionComposer,
    ConfigSectionSidebar,
    ConfigSectionHistory,
    ConfigSectionMcp,
    ConfigSectionFleet,
    ConfigSectionWorkflow,
    ConfigSectionSession,
    ConfigSectionLegacy,
    ConfigSectionExperimental,
    ConfigScopeSession,
    ConfigScopeSaved,
    ConfigCommandSource,
    ConfigCommandInvalidValue,
    ConfigSearchUpdated,
    ConfigPromptSuggestionUpdated,
    ConfigLabelNotificationQuiet,
    ConfigLabelNotificationSound,
    ConfigLabelNotificationCondition,
    ConfigLabelNotificationMethod,
    ConfigLabelNotificationThreshold,
    ConfigLabelNotificationSummary,
    ConfigLabelNotificationSubagents,
    ConfigLabelNotificationTurnComplete,
    ConfigLabelNotificationSubagentTerminal,
    ConfigLabelNotificationApprovalNeeded,
    ConfigLabelNotificationInputNeeded,
    ConfigLabelNotificationElevationNeeded,
    ConfigLabelNotificationModelNotify,
    ConfigLabelNotificationCompletionSound,
    ConfigLabelNotificationSoundFile,
    ConfigLabelNotificationEventSoundEnabled,
    ConfigLabelNotificationEventSoundEvents,
    ConfigLabelNotificationEventSoundInterval,
    ConfigLabelNotificationEventSoundQuiet,
    ConfigHintNotificationPolicy,
    ConfigHintNotificationSound,
    ConfigHintNotificationLegacy,
    ConfigChoiceNotificationWhale,
    ConfigChoiceNotificationLegacy,
    ConfigChoiceNotificationAlways,
    ConfigChoiceNotificationUnfocused,
    ConfigChoiceNotificationNever,
    ConfigChoiceNotificationFile,
    ConfigChoiceNotificationBell,
    ConfigNotificationsSetHint,
    ConfigNotificationUpdated,
    ConfigNotificationsWholeNumber,
    ConfigAuditSearchProvider,
    ConfigAuditPromptSuggestion,
    ConfigAuditNotifications,
    ConfigHelpDiscoverable,
    ConfigEditCancelled,
    ConfigEditTitlePrefix,
    ConfigEditScopeLabel,
    ConfigEditCurrentLabel,
    ConfigEditHintLabel,
    ConfigEditNewLabel,
    ConfigEditFooter,
    ConfigLocalePartialBadge,
    ConfigLocalePartialDetail,
    ConfigRowEffective,
    ConfigDefaultValue,
    ConfigDefaultReasoning,
    ConfigUnavailable,
    ConfigLabelProvider,
    ConfigLabelBaseUrlDeepseek,
    ConfigLabelProviderUrl,
    ConfigHintProviderUrl,
    ConfigLabelModel,
    ConfigLabelFastModel,
    ConfigLabelDefaultModel,
    ConfigLabelReasoningEffort,
    ConfigLabelApprovalMode,
    ConfigLabelPermissionPosture,
    ConfigLabelApprovalPolicy,
    ConfigLabelManagedApprovalPolicy,
    ConfigLabelDefaultMode,
    ConfigLabelAllowShell,
    ConfigLabelManagedAllowShell,
    ConfigLabelTelemetry,
    ConfigHintTelemetry,
    ConfigValueTelemetryOn,
    ConfigValueTelemetryOff,
    ConfigLabelStreamTimeout,
    ConfigLabelTheme,
    ConfigLabelLocale,
    ConfigLabelBackground,
    ConfigLabelWorkSurfacePlacement,
    ConfigLabelTopHeight,
    ConfigLabelSideWidth,
    ConfigLabelCalmMode,
    ConfigLabelLowMotion,
    ConfigLabelFancyAnimations,
    ConfigLabelShowThinking,
    ConfigLabelThinkingHighlight,
    ConfigLabelShowToolDetails,
    ConfigLabelInlineDiffs,
    ConfigLabelStatusIndicator,
    ConfigLabelSynchronizedOutput,
    ConfigLabelCostCurrency,
    ConfigLabelTranscriptSpacing,
    ConfigLabelToolCollapse,
    ConfigLabelComposerDensity,
    ConfigLabelComposerBorder,
    ConfigLabelComposerMultilineMode,
    ConfigLabelComposerVimMode,
    ConfigLabelBracketedPaste,
    ConfigLabelPasteBurstDetection,
    ConfigLabelMentionMenuLimit,
    ConfigLabelMentionMenuBehavior,
    ConfigLabelMentionWalkDepth,
    ConfigLabelWorkspaceFollowSymlinks,
    ConfigLabelContextPanel,
    ConfigLabelSessionsRail,
    ConfigLabelSessionAutoResume,
    ConfigLabelAutoCompact,
    ConfigLabelAutoCompactThreshold,
    ConfigLabelMaxHistory,
    ConfigLabelMcpOpen,
    ConfigLabelMcpReconnect,
    ConfigLabelMcpDiagnose,
    ConfigLabelPluginsOpen,
    ConfigLabelMcpConfigPath,
    ConfigLabelFleetSpawnDepth,
    ConfigLabelGoalCommand,
    ConfigLabelWorkflow,
    ConfigLabelFeaturePrefix,
    ConfigColumnSetting,
    ConfigColumnValue,
    ConfigColumnScope,
    ConfigActionOpenProvider,
    ConfigActionOpenModel,
    ConfigActionOpenMcp,
    ConfigActionMcpReconnect,
    ConfigActionMcpDiagnose,
    ConfigActionOpenPlugins,
    ConfigActionToggle,
    ConfigActionChoose,
    ConfigActionEdit,
    ConfigActionReadOnly,
    ModelPickerAutoNetworkHint,
    ModelPickerAutoNetworkActiveProviderHint,
    ModelPickerAutoLocalHint,
    ModelPickerAutoLastRoute,
    AutoRouteSelectedToast,
    ModelPickerNotListed,
    HelpGroupCommonCommands,
    HelpGroupAllCommands,
    HelpTitle,
    HelpSubtitle,
    HelpFilterPlaceholder,
    HelpFilterPrefix,
    HelpNoMatches,
    HelpSlashCommands,
    HelpKeybindings,
    HelpUserCommands,
    HelpSkills,
    HelpFooterTypeFilter,
    HelpFooterMove,
    HelpFooterJump,
    HelpFooterClose,
    CmdAttachDescription,
    CmdAnchorDescription,
    CmdCacheDescription,
    CmdPreviewRequestDescription,
    CmdToolsDescription,
    CmdTurnInspectDescription,
    CmdChangeDescription,
    CmdEffortDescription,
    CmdChangeHeader,
    CmdChangeTranslationQueued,
    CmdChangeTranslationUnavailable,
    CmdChangePreviousVersion,
    CmdBalanceDescription,
    CmdImportClaudeDescription,
    CmdClearDescription,
    CmdCompactDescription,
    CmdPurgeDescription,
    CmdConfigDescription,
    CmdPermissionsDescription,
    PermissionsListHeader,
    PermissionsNoRules,
    PermissionsFileMissing,
    PermissionsFileEmpty,
    PermissionsFilePresent,
    PermissionsRuleEntry,
    PermissionsMatchExactCommand,
    PermissionsMatchCommandPrefix,
    PermissionsMatchExactPath,
    PermissionsMatchAnyInvocation,
    PermissionsScopeGlobal,
    PermissionsScopeRepo,
    PermissionsAppliesHere,
    PermissionsInactiveHere,
    PermissionsRemovePreview,
    PermissionsRemoved,
    PermissionsUsage,
    PermissionsRuleNotFound,
    AutoReviewReceiptGuardianAllowed,
    AutoReviewReceiptGuardianDenied,
    AutoReviewReceiptGuardianUnavailable,
    AutoReviewReceiptDeterministicBlocked,
    AutoReviewReceiptHeld,
    FooterHintEscInterrupt,
    /// `{enter}` and `{steer}` are key names composed in code.
    PostureHintEnterAgain,
    PermissionsPostureHeader,
    PermissionsPostureAsk,
    PermissionsPostureAuto,
    PermissionsPostureBypass,
    PermissionsPostureNever,
    PermissionsReceiptsNote,
    PermissionsOperationFailed,
    CmdAuthDescription,
    CmdConstitutionDescription,
    CmdContextDescription,
    CmdCostDescription,
    CmdReceiptsDescription,
    CmdDiffDescription,
    CmdEditDescription,
    CmdExitDescription,
    CmdExportDescription,
    CmdCopyDescription,
    CmdCopyNoOutput,
    CmdCopySuccess,
    CmdCopySuccessNoBackup,
    CmdCopyQueued,
    CmdCopyQueuedNoBackup,
    CmdCopyFailed,
    CmdCopyFailedNoBackup,
    CmdFeedbackDescription,
    FeedbackNoSession,
    FeedbackUnavailable,
    FeedbackReviewNotice,
    FeedbackHelp,
    FeedbackDraftRequested,

    CmdHfDescription,
    CmdHelpDescription,
    CmdProfileDescription,
    ProfileUsage,
    ProfileSwitching,
    ProfileSwitched,
    ProfileStatus,
    ProfileSwitchFailed,
    CmdHomeDescription,
    CmdOverviewDescription,
    HomeBackToConversation,
    HomeNavigationBusy,
    CmdHooksDescription,
    CmdAgentDescription,
    CmdGoalDescription,
    GoalReceiptSet,
    GoalReceiptSetOperate,
    GoalControlAccepted,
    GoalControlRuntimeUnavailable,
    GoalStatusIdleHint,
    GoalContinuationWaiting,
    GoalContinuationReady,
    GoalContinuationStopped,
    CmdInitDescription,
    CmdJobsDescription,
    CmdDispatchDescription,
    CmdLinksDescription,
    CmdLoadDescription,
    CmdLogoutDescription,
    CmdLoginDescription,
    CmdMcpDescription,
    McpRecommendedUnknownId,
    McpRecommendationsHeading,
    McpRecommendationsSafety,
    McpRecommendationGithub,
    McpRecommendationChrome,
    McpRecommendationPlaywright,
    McpRecommendationContainerUse,
    McpCapabilitiesAdvertised,
    McpCapabilitiesLegacyFallback,
    McpCapabilitiesNotObserved,
    CmdMemoryDescription,
    CmdPluginDescription,
    ExtensionsActionAdd,
    ExtensionsActionEnable,
    ExtensionsActionDisable,
    ExtensionsActionReview,
    ExtensionsActionRemove,
    ExtensionsStateOn,
    ExtensionsStateOff,
    ExtensionsStateNeedsReview,
    ExtensionsReviewExplainer,
    ExtensionsScopeGlobal,
    ExtensionsScopeProject,
    ExtensionsScopePlugin,
    ExtensionsRemoveArmed,
    ExtensionsActionEdit,
    ExtensionsHooksAddLabel,
    ExtensionsHooksAddDescription,
    ExtensionsActionReload,
    ExtensionsActionConnect,
    ExtensionsActionReconnect,
    ExtensionsActionReauth,
    ExtensionsActionDiagnose,
    ExtensionsActionManage,
    ExtensionsActionFocus,
    ExtensionsActionFold,
    ExtensionsActionTabs,
    ExtensionsCompatibilityFull,
    ExtensionsCompatibilityPartial,
    ExtensionsGroupBuiltIn,
    ExtensionsGroupConfigured,
    ExtensionsGroupProblems,
    ExtensionsGroupRecommended,
    ExtensionsGroupServers,
    ExtensionsGroupNeedsAttention,
    ExtensionsGroupNeedsLogin,
    ExtensionsGroupStatus,
    ExtensionsGroupUser,
    ExtensionsGroupWorkspace,
    ExtensionsHookDetail,
    ExtensionsHookFallback,
    ExtensionsHooksConfiguration,
    ExtensionsInventoryAgents,
    ExtensionsInventoryCommands,
    ExtensionsInventoryHooks,
    ExtensionsInventoryMcp,
    ExtensionsInventoryNone,
    ExtensionsInventorySkills,
    ExtensionPromptUnavailable,
    ProfileConstitutionUnavailableLocal,
    ExtensionsMarketplaceDetail,
    ExtensionsMarketplaceUnavailable,
    ExtensionsMcpEmpty,
    ExtensionsMcpBrowse,
    ExtensionsMcpDetail,
    ExtensionsMcpNotInspected,
    ExtensionsMcpNotConnectedYet,
    ExtensionsMcpRefresh,
    ExtensionsMcpSummary,
    ExtensionsNoItems,
    ExtensionsNoMatches,
    ExtensionsPluginDetail,
    ExtensionsProductChromeDescription,
    ExtensionsProductDetail,
    ExtensionsProductPlaywrightDescription,
    ExtensionsProductCodewhaleComputerUseDescription,
    ExtensionsStateFirstParty,
    ExtensionsSearchLabel,
    ExtensionsSkillRootCompatibleGlobal,
    ExtensionsSkillRootCompatibleProject,
    ExtensionsSkillRootConfigured,
    ExtensionsSkillRootGlobal,
    ExtensionsSkillRootProject,
    ExtensionsSkillRootRegistryCache,
    ExtensionsSkillRootReviewedPlugin,
    ExtensionsStateAvailable,
    ExtensionsStateConnected,
    ExtensionsStateEnabled,
    ExtensionsStateError,
    ExtensionsStateInactive,
    ExtensionsStateInapplicable,
    ExtensionsStateInvalid,
    ExtensionsStateNotInspected,
    ExtensionsStateDisconnected,
    ExtensionsStateRejected,
    ExtensionsStateReviewedCandidate,
    ExtensionsStateUnderEvaluation,
    ExtensionsStateUnstaged,
    ExtensionsStateUnsupported,
    ExtensionsStateWarning,
    ExtensionsTabHooks,
    ExtensionsTabMarketplace,
    ExtensionsTabMarketplaceCompact,
    ExtensionsTabPlugins,
    ExtensionsTierCommunity,
    ExtensionsTierCurated,
    ExtensionsTierOfficial,
    ExtensionsTierPartner,
    ExtensionsTitle,
    ExtensionsTrustCapabilitiesChanged,
    ExtensionsTrustContentChanged,
    ExtensionsTrustNotReviewed,
    ExtensionsTrustTrusted,
    ExtensionsValueNo,
    ExtensionsValueYes,
    PluginKimiUsage,
    PluginKimiManagedRootHeading,
    PluginKimiNoneFound,
    PluginKimiLicenseUnspecified,
    PluginKimiApplicable,
    PluginKimiNotApplicable,
    PluginKimiCandidateSummary,
    PluginKimiCandidateDetails,
    PluginKimiRejectedHeading,
    PluginKimiInspectionFooter,
    PluginKimiCandidateMissing,
    PluginKimiCandidateChanged,
    PluginKimiHomeMissing,
    PluginKimiRootInspectFailed,
    PluginKimiRootMustBeDirectory,
    PluginKimiRootCanonicalizeFailed,
    PluginKimiRootListFailed,
    PluginKimiEntryReadFailed,
    PluginKimiEntryLimit,
    PluginKimiEntryInspectFailed,
    PluginKimiEntryLinksRefused,
    PluginKimiEntryOutsideRoot,
    PluginKimiEntryCanonicalizeFailed,
    PluginKimiManifestUnreadable,
    PluginKimiManifestMustBeFile,
    PluginKimiManifestInvalid,
    PluginKimiDirectoryNameMismatch,
    PluginKimiHashUnavailable,
    PluginKimiRollbackDestinationMissing,
    PluginKimiMismatchRemoved,
    PluginKimiMismatchRollbackFailed,
    PluginKimiUserPluginDirectory,
    PluginKimiMarketplaceZipUnsupported,
    PluginKimiMarketplaceRemoteUnsupported,
    PluginKimiMarketplaceGzipTarball,
    CmdPluginBundleUsage,
    CmdPluginBundleNoneFound,
    CmdPluginBundleListHeader,
    CmdPluginLegacyListHeader,
    CmdPluginBundleNotFound,
    CmdPluginBundleReloaded,
    PluginPromptSuggestTrust,
    PluginPromptSuggestEnable,
    PluginPromptSuggestMarketplace,
    PluginCtaInstallPrompt,
    PluginCtaReview,
    PluginCtaInstall,
    PluginCtaReviewTrust,
    PluginCtaEnable,
    PluginCtaDismiss,
    PluginCtaDismissSaveFailed,
    PluginSuggestionReason,
    PagerActionConfirm,
    CmdPluginBundleDetail,
    CmdPluginOwnerReport,
    CmdPluginOwnerActivating,
    CmdPluginOwnerActive,
    CmdPluginOwnerFailed,
    CmdPluginOwnerFaulted,
    CmdPluginOwnerRevoked,
    CmdPluginOwnerInactive,
    CmdPluginBundleDiagnosticsHeader,
    CmdPluginBundleMutationSuccess,
    CmdPluginActionFailed,
    CmdPluginNoneFound,
    CmdPluginNotFound,
    CmdPluginListHeader,
    CmdPluginDetailDescription,
    CmdPluginDetailSchema,
    CmdPluginDetailApproval,
    CmdPluginDetailPath,
    CmdModeDescription,
    CmdModelDescription,
    CmdModelsDescription,
    CmdRouterDescription,
    RouterSetupTitle,
    RouterPresetJevHint,
    RouterPresetFastHint,
    RouterPresetOffHint,
    RouterPresetOffCostSavingHint,
    RouterPresetCustomHint,
    RouterPresetUnavailable,
    RouterTypesafeSignupPaused,
    RouterTestSaveHint,
    RouterTestHeader,
    RouterTestNotMade,
    RouterTestWouldRoute,
    RouterTestDecision,
    RouterTestChoice,
    RouterTestCostReported,
    RouterTestCostUnreported,
    RouterTestUsage,
    RouterTestFailing,
    RouterSaved,
    RouterNotSaved,
    RouterRuntimeNotReloaded,
    RouterCurrent,
    RouterCurrentFailing,
    RouterSummary,
    RouterSummaryThinking,
    RouterPresetFastLabel,
    RouterPresetCustomLabel,
    RouterNoKey,
    RouterNoFastTier,
    RouterNeedKey,
    RouterCustomByHand,
    RouterCustomHelpIntro,
    RouterCustomHelpOutro,
    RouterActionTest,
    RouterActionSave,
    RouterActionDiscard,
    ModelsListHeader,
    ModelsListHint,
    ModelsUpdateSummary,
    ModelsUpdatePartial,
    ModelsCodexHint,
    ModelsSourceFallback,
    CmdModelDbDescription,
    CmdNetworkDescription,
    CmdUpdateDescription,
    CmdNoteDescription,
    CmdThemeDescription,
    CmdProviderDescription,
    CmdQueueDescription,
    CmdQueueUsage,
    CmdQueueDraftHeader,
    CmdQueueNoMessages,
    CmdQueueListHeader,
    CmdQueueTip,
    CmdQueueAlreadyEditing,
    CmdQueueNotFound,
    CmdQueueEditingStatus,
    CmdQueueEditingMessage,
    CmdQueueDropped,
    CmdQueueAlreadyEmpty,
    CmdQueueCleared,
    CmdQueueMissingIndex,
    CmdQueueIndexPositive,
    CmdQueueIndexMin,
    CmdRelayDescription,
    CmdRemoteControlDescription,
    CmdRemoteEnvDescription,
    CmdRemoteEnvOverview,
    CmdRemoteEnvOpening,
    CmdRemoteEnvUnavailable,
    CmdRemoteEnvSourceCustodyPolicy,
    CmdRemoteEnvBrowserLabel,
    CmdRenameDescription,
    CmdTitleDescription,
    CmdRestoreDescription,
    CmdRetryDescription,
    CmdReviewDescription,
    CmdRlmDescription,
    CmdSaveDescription,
    CmdFullscreenDescription,
    CmdInlineDescription,
    CmdForkDescription,
    CmdNewDescription,
    CmdSessionsDescription,
    CmdTreeDescription,
    CmdBranchDescription,
    CmdResumeDescription,
    CmdSettingsDescription,
    CmdSidebarDescription,
    CmdSkillDescription,
    CmdSkillsDescription,
    CmdStashDescription,
    CmdStatusDescription,
    CmdStatuslineDescription,
    CmdStructcopyDescription,
    CmdStructcopyKindTurn,
    CmdStructcopyKindTool,
    CmdStructcopyKindPlan,
    CmdStructcopyKindWorkflow,
    CmdStructcopyUsageError,
    CmdStructcopyUnavailable,
    CmdStructcopyBusy,
    CmdStructcopyPrepareFailed,
    CmdStructcopyClipboardQueued,
    CmdStructcopyClipboardAccepted,
    CmdStructcopyClipboardFailed,
    CmdStructcopyReceiptTooLarge,
    CmdFleetDescription,
    PetUnobserved,
    PetOffline,
    PetDozing,
    PetWatchUnavailable,
    PetWatchRestored,
    PetWatchStorageUnavailable,
    PetWatchExported,
    PetWatchExportFailed,
    PetWatchExportQueued,
    PetWatchExportUnavailable,
    PetHabitatTitle,
    PetHabitatHints,
    PetHabitatQueued,
    PetWatchSoundOn,
    PetWatchSoundOff,
    PetWatchSoundPaused,
    PetWatchSoundUnavailable,
    CmdPetDescription,
    ConfigLabelPetMode,
    ConfigHintPetMode,
    PetModeCopyHint,
    PetModeOn,
    PetModeOff,
    PetModeOnLabel,
    PetModeInspectHint,
    PetModeReplyTitle,
    PetModeMessageHint,
    PetModePaneHint,
    PetModeAgentHints,
    PetModeReplyHints,
    PetModeCompanionHints,
    PetModeOffLabel,
    PetViewOpen,
    PetViewClosed,
    CmdLaneDescription,
    CmdWorkflowDescription,
    CmdWorkflowsDescription,
    CmdAutoDescription,
    AutoReceiptOn,
    AutoReceiptPlanNote,
    CmdHotbarDescription,
    CmdSetupDescription,
    CmdSubagentsDescription,
    CmdAdvisorDescription,
    CmdSystemDescription,
    CmdAutomationDescription,
    CmdTaskDescription,
    CmdTokensDescription,
    CmdTranslateDescription,
    CmdTranslateOff,
    CmdTranslateOn,
    TranslationInProgress,
    TranslationComplete,
    TranslationFailed,
    CmdTrustDescription,
    CmdLspDescription,
    CmdShareDescription,
    CmdWorkspaceDescription,
    CmdUndoDescription,
    CmdVerboseDescription,
    CmdCacheAdvice,
    CmdCacheFootnote,
    CmdCacheHeader,
    CmdCacheNoData,
    CmdCacheSessionRates,
    CmdCacheRateParent,
    CmdCacheRateAgents,
    CmdCacheRateCombined,
    CmdCacheTotals,
    CmdCostReport,
    CmdCostReportSubtotal,
    CmdCostReportUnknown,
    CmdCostUnknownValue,
    CmdCostEstimateOnly,
    CmdCostCoverage,
    CmdCostCoverageUnknownLegacy,
    CmdCostUnpricedTurns,
    CmdCostUnpricedClasses,
    CmdCostPricingProvenance,
    CmdCostLivePricingDowngraded,
    CmdCostLivePricingUnavailable,
    CmdCostRoutesHeader,
    CmdTokensCacheWriteTotal,
    CmdTokensCacheBoth,
    CmdTokensCacheHitOnly,
    CmdTokensCacheMissOnly,
    CmdTokensContextUnknownWindow,
    CmdTokensContextWithWindow,
    CmdTokensNotReported,
    CmdTokensReport,
    FooterAgentSingular,
    FooterAgentsPlural,
    HeaderAgentsChip,
    FooterPressCtrlCAgain,
    FooterWorking,
    FooterBalancePrefix,
    HelpSectionActions,
    HelpSectionClipboard,
    HelpSectionPointer,
    HelpSectionEditing,
    HelpSectionHelp,
    HelpSectionModes,
    HelpSectionNavigation,
    HelpSectionSessions,
    KbScrollTranscript,
    KbNavigateHistory,
    KbScrollTranscriptAlt,
    KbBrowseHistory,
    KbScrollPage,
    KbJumpTopBottom,
    KbJumpTopBottomEmpty,
    KbJumpToolBlocks,
    KbMoveCursor,
    KbJumpLineStartEnd,
    KbDeleteChar,
    KbDeleteWord,
    KbYank,
    KbToggleFileTree,
    KbSelectText,
    KbSelectAllDraft,
    KbClearDraft,
    KbRestoreClearedDraft,
    KbStashDraft,
    KbSearchHistory,
    KbInsertNewline,
    KbSendDraft,
    KbSteerCurrentTurn,
    KbCloseMenu,
    KbCancelOrExit,
    KbShellControls,
    KbExitEmpty,
    KbCommandPalette,
    KbSettings,
    KbCancelBackgroundShellJobs,
    KbFuzzyFilePicker,
    KbCompactInspector,
    KbCompactContext,
    KbLastMessagePager,
    KbSelectedDetails,
    KbToolDetailsPager,
    KbReasoningDetail,
    KbTurnInspector,
    KbExternalEditor,
    KbLiveTranscript,
    KbBacktrackMessage,
    KbCompleteCycleModes,
    KbCycleThinking,
    KbCyclePermissions,
    KbJumpPlanAgentYolo,
    KbAltJumpPlanAgentYolo,
    KbFocusSidebar,
    KbSessionPicker,
    KbUpdateInstall,
    /// Startup hint: the running version is newer than the last-launched one.
    UpdateChangedHint,
    KbTerminalPaste,
    KbPasteAttach,
    KbCopySelection,
    ClipboardSshPasteHint,
    /// Copy receipt when a native clipboard confirmed the write.
    ClipboardCopied,
    /// Copy receipt when the text went to the terminal (OSC 52 / tmux),
    /// which never acknowledges it.
    ClipboardSentToTerminal,
    /// Cut when only the terminal took the copy: the text is kept, because
    /// the terminal never confirms it.
    ClipboardCutKeptText,
    /// Paste found nothing: the clipboard read came back empty or failed.
    ClipboardNothingToPaste,
    ComposerOversizedSubmitHeld,
    /// Short toast form of [`Self::ComposerOversizedSubmitHeld`]; the toast row
    /// sheds clauses to fit, so the full text goes to the transcript instead.
    ComposerOversizedSubmitHeldShort,
    KbContextMenu,
    KbPointerScroll,
    KbPointerClick,
    KbPointerDrag,
    KbAttachPath,
    KbHelpOverlay,
    KbCycleWorkDock,
    KbCycleWorkDockBack,
    KbToggleHelp,
    KbToggleHelpSlash,
    HelpUsageLabel,
    HelpAliasesLabel,
    SettingsTitle,
    SettingsConfigFile,
    SettingsTuiPrefsFolded,
    SettingsTuiPrefsKept,
    SettingsTuiPrefsQuarantined,
    ClearConversation,
    ClearConversationBusy,
    ModelChanged,
    ModelChangedSessionNote,
    LinksProjectTitle,
    LinksDocumentation,
    LinksCommunity,
    LinksGitHub,
    LinksManagedApp,
    LinksManagedAppNote,
    LinksTitle,
    LinksDashboard,
    LinksDocs,
    LinksKimiCodeRouteNote,
    LinksTip,
    WorkspaceCurrent,
    WorkspaceNotFound,
    WorkspaceNotDirectory,
    WorkspaceSwitching,
    WorkspaceHomeUnresolved,
    WorkspaceSwitchBusy,
    WorkspaceUnchanged,
    WorkspaceSwitched,
    WorkspaceStatus,
    SubagentsFetching,
    SubagentsNoCurrentSessionFleetWorkers,
    SubagentsCurrentSessionFleetWorkersTitle,
    SubagentsCurrentSessionFleetWorkerRoles,
    SubagentsCurrentSessionFleetWorkersStatus,
    SubagentsEmptyGuidance,
    SubagentsStatusRunning,
    SubagentsStatusCompleted,
    SubagentsStatusInterrupted,
    SubagentsStatusFailed,
    SubagentsStatusCancelled,
    SubagentsRowStatusInterrupted,
    SubagentsRowStatusCancelled,
    SubagentsRowStatusBudgetExhausted,
    SubagentsSummaryItem,
    SubagentsGroupHeading,
    SubagentsHeaderRoster,
    SubagentsHeaderColumns,
    SubagentsActionRefresh,
    SubagentsActionRosterSetup,
    SubagentsLabelReason,
    SubagentsLabelRole,
    SubagentsLabelPosture,
    SubagentsLabelGit,
    SubagentsLabelObjective,
    SubagentsLabelResult,
    SubagentsPostureDetails,
    SubagentsValueOn,
    SubagentsValueOff,
    SubagentsShellNone,
    SubagentsShellReadOnly,
    SubagentsShellFull,
    SubagentsBranch,
    SubagentsBranchWithWorkspace,
    SubagentsRoleWorker,
    SubagentsRoleScout,
    SubagentsRolePlanner,
    SubagentsRoleBuilder,
    SubagentsRoleVerifier,
    SubagentsRoleReviewer,
    SubagentsRoleConsultant,
    SubagentsRoleCustom,
    HelpUnknownCommand,
    HomeDashboardTitle,
    HomeModel,
    HomeMode,
    HomeWorkspace,
    HomeHistory,
    HomeTokens,
    HomeQueued,
    HomeSubagents,
    HomeSkill,
    HomeQuickActions,
    HomeQuickLinks,
    HomeQuickSkills,
    HomeQuickConfig,
    HomeQuickSettings,
    HomeQuickModel,
    HomeQuickSubagents,
    HomeQuickTaskList,
    HomeQuickHelp,
    HomeQuickWorkspace,
    HomeQuickRestore,
    HomeQuickTokens,
    HomeModeTips,
    HomeAgentModeTip,
    HomeAgentModeReviewTip,
    HomeAgentModeYoloTip,
    HomeYoloModeTip,
    HomeYoloModeCaution,
    HomePlanModeTip,
    HomePlanModeChecklistTip,
    HomeOperateModeTip,
    HomeOperateModeFleetTip,
    HomeGoalModeTip,
    // Onboarding screens — calm first-run welcome (#3938 rewrite).
    OnboardWelcomeTitle,
    OnboardWelcomeLead,
    OnboardWelcomeBegin,
    OnboardActionBack,
    OnboardActionExit,
    OnboardStepsTitle,
    // Onboarding screens — language picker.
    OnboardLanguageTitle,
    OnboardLanguageBlurb,
    OnboardLanguagePick,
    OnboardLanguageKeep,
    OnboardProviderTitle,
    OnboardProviderBlurb,
    OnboardProviderChoose,
    OnboardProviderOffline,
    KimiCodePlanApiKeyHint,
    KimiCodePlanRouteHint,
    KimiCodePlanNoImportHint,
    StepfunBillingRouteTitle,
    StepfunBillingRouteIntro,
    StepfunBillingRoutePaygOption,
    StepfunBillingRoutePlanOption,
    StepfunPlanApiKeyHint,
    StepfunPlanRouteHint,
    OnboardApiKeyRejectedEnv,
    // Onboarding screens — workspace trust prompt.
    OnboardTrustTitle,
    OnboardTrustQuestion,
    OnboardTrustLocationPrefix,
    OnboardTrustRiskHint,
    OnboardTrustEffectHint,
    OnboardTrustActionTrust,
    OnboardTrustActionSkip,
    OnboardTrustActionQuit,
    OnboardTrustEnterHint,
    OnboardTrustUntrustedNotice,
    RedactionGateSaveFailed,
    RedactionGateTitle,
    RedactionGateQuestion,
    RedactionGateDangerNotice,
    RedactionGateRisk,
    RedactionGateEffect,
    RedactionGateRollbackHint,
    RedactionGateEnterHint,
    RedactionGateActionConfirm,
    RedactionGateActionKeep,
    RedactionGateActionQuit,
    // Startup gate — second-stage final confirmation before the opt-out
    // really takes effect.
    RedactionGateConfirmTitle,
    RedactionGateConfirmQuestion,
    RedactionGateActionBack,
    // Onboarding screens — explicit offline ("explore") choice (#3927).
    OnboardOfflineOption,
    OnboardOfflineNotice,
    // Onboarding screens — ready screen and the seeded first task.
    OnboardReadyTitle,
    OnboardReadyLead,
    OnboardReadyStart,
    OnboardReadyCustomize,
    OnboardSeedCodeProject,
    OnboardSeedFolder,
    // Constitution-first setup wizard.
    SetupWizardTitle,
    SetupWizardWhy,
    SetupWizardProgress,
    SetupActionBack,
    SetupActionContinue,
    SetupActionSkip,
    SetupActionRetry,
    SetupActionScrollBody,
    SetupActionGuided,
    SetupActionTuneGuided,
    SetupActionModelDraft,
    SetupActionFreeform,
    SetupActionKeepExisting,
    SetupActionUseRecommended,
    SetupActionCustomize,
    SetupActionProvider,
    SetupActionModel,
    SetupActionFleet,
    SetupActionHotbar,
    SetupActionRemote,
    SetupActionMode,
    SetupActionConfig,
    SetupActionRuntimePreset,
    SetupActionApplyRuntimePreset,
    SetupActionUseBundled,
    SetupActionDefer,
    SetupActionCancel,
    SetupStatusNotStarted,
    SetupStatusRecommended,
    SetupStatusOptional,
    SetupStatusDeferred,
    SetupStatusInProgress,
    SetupStatusNeedsAction,
    SetupStatusVerified,
    SetupStatusSkipped,
    SetupStatusFailed,
    SetupStepLanguageTitle,
    SetupStepLanguageWhy,
    SetupStepProviderModelTitle,
    SetupStepProviderModelWhy,
    SetupStepTrustSandboxTitle,
    SetupStepTrustSandboxWhy,
    SetupStepOperateFleetTitle,
    SetupStepOperateFleetWhy,
    SetupStepToolsMcpTitle,
    SetupStepToolsMcpWhy,
    SetupStepHotbarTitle,
    SetupStepHotbarWhy,
    SetupStepRemoteRuntimeTitle,
    SetupStepRemoteRuntimeWhy,
    SetupStepPersistenceTitle,
    SetupStepPersistenceWhy,
    SetupStepConstitutionTitle,
    SetupStepConstitutionWhy,
    SetupStepVerificationTitle,
    SetupStepVerificationWhy,
    SetupCheckpointLayerOrder,
    SetupCheckpointDoneBundled,
    SetupCheckpointDoneGuided,
    SetupCheckpointDoneKept,
    SetupCheckpointDeferred,
    SetupStepSkipped,
    SetupStepRetryRecorded,
    SetupLanguageReviewed,
    SetupConstitutionChoiceLabel,
    SetupConstitutionSourceLabel,
    SetupConstitutionValidityLabel,
    SetupConstitutionPreviewLabel,
    SetupConstitutionExistingLabel,
    SetupConstitutionExpertOverrideLabel,
    SetupConstitutionGuidedHint,
    SetupConstitutionGuidedAnswersHint,
    SetupConstitutionExistingDefaultDetail,
    SetupConstitutionRepairDefaultDetail,
    SetupConstitutionPurposeLabel,
    SetupConstitutionAutonomyLabel,
    SetupConstitutionEvidenceLabel,
    SetupConstitutionCommunicationLabel,
    SetupConstitutionPrivacyLabel,
    SetupConstitutionPrinciplesLabel,
    SetupCardRouteLabel,
    SetupCardModelLabel,
    SetupCardAuthLabel,
    SetupCardHealthLabel,
    SetupCardIntentLabel,
    SetupCardApprovalLabel,
    SetupCardShellLabel,
    SetupCardTrustLabel,
    SetupCardSandboxLabel,
    SetupCardNetworkLabel,
    SetupOperateRuntimeLabel,
    SetupOperateRosterLabel,
    SetupOperateConcurrencyLabel,
    SetupOperateReadinessLabel,
    SetupOperateReviewHint,
    SetupOperateReviewed,
    SetupOperateNeedsActionSaved,
    SetupHotbarBindingsLabel,
    SetupHotbarActionsLabel,
    SetupHotbarReviewHint,
    SetupHotbarReviewed,
    SetupToolsMcpServersLabel,
    SetupToolsMcpSkillsLabel,
    SetupToolsMcpToolsLabel,
    SetupToolsMcpPluginsLabel,
    SetupToolsMcpHotbarLabel,
    SetupToolsMcpReviewHint,
    SetupToolsMcpReviewed,
    SetupToolsMcpNeedsActionSaved,
    SetupToolsMcpPreviewTitle,
    SetupToolsMcpOnRampText,
    SetupToolsMcpDshLabel,
    SetupToolsMcpDshRow,
    SetupRemoteCloudsLabel,
    SetupRemoteBridgesLabel,
    SetupRemoteProvidersLabel,
    SetupRemoteModeLabel,
    SetupRemoteModeLocalOnly,
    SetupRemoteModeRuntimeApi,
    SetupRemoteModeMobileLan,
    SetupRemoteModeChatBridge,
    SetupRemoteStatusDisabled,
    SetupRemoteStatusReady,
    SetupRemoteStatusNeedsAction,
    SetupRemoteReviewHint,
    SetupRemotePreviewTitle,
    SetupRemoteReviewed,
    SetupPersistenceHomeLabel,
    SetupPersistenceConfigLabel,
    SetupPersistenceStateLabel,
    SetupPersistenceConstitutionLabel,
    SetupPersistenceMemoryLabel,
    SetupPersistenceNotesLabel,
    SetupPersistenceReviewHint,
    SetupPersistenceReviewed,
    SetupProviderModelReadyHint,
    SetupProviderModelNeedsActionHint,
    SetupProviderModelReviewed,
    SetupProviderModelNeedsActionSaved,
    SetupRuntimePostureBoundary,
    SetupProjectPermissionsInvalid,
    SetupProjectPermissionsOverride,
    SetupRuntimePostureReviewHint,
    SetupRuntimePostureReviewed,
    SetupRuntimePresetSelectedLabel,
    SetupRuntimePresetDiffLabel,
    SetupRuntimePresetAskFirstTitle,
    SetupRuntimePresetAskFirstDescription,
    SetupRuntimePresetNormalAgentTitle,
    SetupRuntimePresetNormalAgentDescription,
    SetupRuntimePresetHighTrustTitle,
    SetupRuntimePresetHighTrustDescription,
    SetupRuntimePresetPreviewTitle,
    SetupRuntimePresetSafetyFloor,
    SetupRuntimePresetApplyHint,
    SetupRuntimePresetApplied,
    SetupRuntimeProjectOverrideLabel,
    SetupRuntimeProjectOverrideNone,
    SetupReportFirstRunLabel,
    SetupReportUpdateLabel,
    SetupReportOperateLabel,
    SetupReportSourceLabel,
    SetupReportAutonomyLabel,
    SetupReportRuntimePostureLabel,
    SetupReportPersisted,
    SetupReportInherited,
    SetupReportReady,
    SetupReportRequired,
    SetupReportOptional,
    SetupReportRowsLabel,
    SetupReportNextActionLabel,
    SetupReportNextActionNone,
    SetupReportNextActionConstitution,
    SetupReportNextActionProvider,
    SetupReportNextActionRuntime,
    SetupReportNextActionOperate,
    SetupReportNextActionRequired,
    SetupReportRecorded,
    // Context menu.
    CtxMenuTitle,
    CtxMenuCopySelection,
    CtxMenuCopySelectionDesc,
    CtxMenuOpenSelection,
    CtxMenuOpenSelectionDesc,
    CtxMenuClearSelection,
    CtxMenuOpenDetails,
    CtxMenuCopyMessage,
    CtxMenuCopyMessageDesc,
    CtxMenuOpenInEditor,
    CtxMenuOpenInEditorDesc,
    CtxMenuShowCell,
    CtxMenuShowCellDesc,
    CtxMenuHideCell,
    CtxMenuHideCellDesc,
    CtxMenuShowHidden,
    CtxMenuShowHiddenDesc,
    CtxMenuPaste,
    CtxMenuPasteDesc,
    CtxMenuCmdPalette,
    CtxMenuCmdPaletteDesc,
    CtxMenuContextInspector,
    CtxMenuContextInspectorDesc,
    CtxMenuHelp,
    CtxMenuHelpDesc,
    /// Right-click menu: pin/unpin the host terminal window into an
    /// always-on-top mini window.
    CtxMenuWindowPin,
    /// Right-click menu: unpin label shown while the window is pinned.
    CtxMenuWindowUnpin,
    /// Right-click menu: description for the window-pin entry.
    CtxMenuWindowPinDesc,
    /// Right-click menu: a work-surface row's own command, `{command}`.
    CtxMenuRunCommand,
    CtxMenuOpen,
    /// Right-click menu on an agent row: the one agent destination.
    CtxMenuFocusAgent,
    CtxMenuFocusAgentDesc,
    CtxMenuCopyId,
    CtxMenuCopyRow,
    CtxMenuStopAgent,
    /// Right-click menu: stop a work item (job, task, workflow run).
    CtxMenuStopWork,
    /// Right-click menu: label an armed destructive row shows until the
    /// second activation runs it.
    CtxMenuConfirmArmed,
    /// Open in editor refused at launch: `{path}` is no longer a regular
    /// file inside the workspace reached without links.
    CtxMenuEditorRefused,
    /// `/pin` command description (always-on-top mini-window toggle).
    CmdPinDescription,
    /// Status toast: host window is now the always-on-top mini window.
    WindowPinActive,
    /// Status toast: host window restored from the pinned mini window.
    WindowPinReleased,
    /// Status toast: window change failed or was not observed before timeout.
    WindowPinFailed,
    // Agent fanout card.
    FanoutCounts,

    // App mode picker (names, hints) and composer vim indicator.
    AppModeAgent,
    AppModeAuto,
    AppModeYolo,
    AppModePlan,
    AppModeOperate,
    AppModeAgentHint,
    AppModeAutoHint,
    AppModePlanHint,
    AppModeYoloHint,
    AppModeOperateHint,
    VimModeNormal,
    VimModeInsert,
    VimModeVisual,

    // Approval dialog — risk badges, category labels, field labels, options.
    ApprovalEffectReadsOnly,
    ApprovalEffectChangesFiles,
    ApprovalRiskDestructive,
    ApprovalEffectRunsCommand,
    ApprovalEffectUsesNetwork,
    ApprovalEffectConnectedApp,
    ApprovalEffectStartsAgent,
    ApprovalEffectUnclassified,
    ApprovalCategorySafe,
    ApprovalCategoryFileWrite,
    ApprovalCategoryShell,
    ApprovalCategoryNetwork,
    ApprovalCategoryMcpRead,
    ApprovalCategoryMcpAction,
    ApprovalCategoryAgent,
    ApprovalCategoryUnknown,
    ApprovalFieldType,
    ApprovalFieldAbout,
    ApprovalFieldImpact,
    ApprovalFieldParams,
    ApprovalOptionApproveOnce,
    ApprovalOptionApproveAlways,
    ApprovalOptionAllowExactRepo,
    ApprovalSaveAskRuleHint,
    ApprovalOptionDeny,
    ApprovalOptionAbortTurn,
    ApprovalBlockTitle,
    ApprovalControlsHint,
    ApprovalOwnerHeader,
    ApprovalGoToAgent,
    ApprovalControlsHintChild,
    PendingApprovalInAgent,
    ApprovalTruncationHint,
    ApprovalAsksWithoutGit,
    ApprovalFullAccessPolicyBlocked,
    ApprovalChooseHint,
    ApprovalChooseAction,
    ApprovalIntentLabel,
    ApprovalMoreLines,
    ApprovalAutoDeniedSession,
    // Sandbox elevation dialog.
    ElevationTitleSandboxDenied,
    ElevationTitleRequired,
    ElevationFieldTool,
    ElevationFieldCmd,
    ElevationFieldReason,
    ElevationImpactHeader,
    ElevationImpactNetwork,
    ElevationImpactWrite,
    ElevationImpactFullAccess,
    ElevationPromptProceed,
    ElevationOptionNetwork,
    ElevationOptionWrite,
    ElevationOptionFullAccess,
    ElevationOptionAbort,
    ElevationOptionNetworkDesc,
    ElevationOptionWriteDesc,
    ElevationOptionFullAccessDesc,
    ElevationOptionAbortDesc,

    // Context compaction status and errors.
    ContextAutoCompacting,
    ContextManualCompacting,
    ContextCompactionQueued,
    ContextCompactionAlreadyRunning,
    ContextCompactionQueueFull,
    ContextCompactionQueueClosed,
    ContextCompactionRouteInvalid,
    CtxInspTitle,
    CtxInspSessionContext,
    CtxInspSystemPrompt,
    CtxInspReferences,
    CtxInspRecentTools,
    CtxInspToolSchemaCosts,
    CtxInspModel,
    CtxInspWorkspace,
    CtxInspSession,
    CtxInspContext,
    CtxInspTranscript,
    CtxInspWorkspaceStatus,
    CtxInspNotSampledYet,
    CtxInspOk,
    CtxInspHigh,
    CtxInspCritical,
    CtxInspIncluded,
    CtxInspAttached,
    CtxInspNotIncluded,
    CtxInspOutputCaptured,
    CtxInspNoOutputYet,
    CtxInspNoSystemPrompt,
    CtxInspNoReferences,
    CtxInspNoToolActivity,
    CtxInspVHint,
    CtxInspCells,
    CtxInspApiMessages,
    CtxInspActive,
    CtxInspCell,
    CtxInspMoreReferences,
    CtxInspStablePrefix,
    CtxInspVolatileWorkingSet,
    CtxInspFirstLine,
    CtxInspTotal,
    CtxInspTextPromptLayers,
    CtxInspSingleTextBlob,
    CtxInspBlocks,
    CtxInspBlock,
    CtxInspTokens,
    CtxInspLayers,
    CtxInspNone,
    CtxInspEmpty,
    CtxInspCacheFriendly,
    CtxInspChangesByTurn,
    CtxInspStablePrefixOnly,
    CtxInspCacheTip,
    // Tool family labels (card headers, sidebar, footer).
    ToolFamilyRead,
    ToolFamilyPatch,
    ToolFamilyRun,
    ToolFamilyFind,
    ToolFamilyDelegate,
    ToolFamilyFanout,
    ToolFamilyRlm,
    ToolFamilyVerify,
    ToolFamilyThink,
    ToolFamilyGeneric,
    // Tool execution receipt labels (card headers).
    ToolReceiptDone,
    ToolReceiptLinesSingular,
    ToolReceiptLinesPlural,
    // Voice commands (/voice, /voice-send, /voice-control)
    CmdVoiceDescription,
    CmdVoiceSendDescription,
    CmdVoiceControlDescription,
    VoiceEnabled,
    VoiceDisabled,
    VoiceSendEnabled,
    VoiceSendDisabled,
    VoiceControlEnabled,
    VoiceControlDisabled,
    VoiceErrNoAuth,
    VoiceErrNoRecorder,
    VoiceErrNetwork,
    VoiceErrEmptySend,
    VoiceErrTooShort,
    VoiceRecording,
    /// Recording ends on its own after a short silence; nothing reads keys
    /// while the capture runs, so the cue names the pause, not a key.
    VoiceRecordingStopHint,
    VoiceProcessing,
    VoiceTranscribed,
    // Notifications (turn/agent completion).
    NotificationApprovalNeeded,
    NotificationInputNeeded,
    NotificationElevationNeeded,
    NotificationDecisionWebHint,
    NotificationTurnFailed,
    NotificationProviderFallback,
    ApprovalNeverPostureBlocked,
    ApprovalTimedOutDenied,
    NotificationWebApproved,
    NotificationWebDenied,
    NotificationInputSubmitFailed,
    NotificationTurnComplete,
    NotificationSubagentComplete,
    NotificationSubagentFailed,
    NotificationSubagentInterrupted,
    NotificationSubagentCancelled,
    NotificationSubagentBudgetExhausted,
    NotificationShellFinished,
    NotificationShellFailed,
    NotificationShellStopped,
    ShellWaitNoActive,
    ShellWaitInteractive,
    ShellWaitUnavailable,
    ShellWaitReleased,
    NotificationTaskFinished,
    NotificationTaskFailed,
    NotificationTaskStopped,
    NotificationBackgroundFinished,
    NotificationBackgroundMixed,
    BackgroundFinishedHeading,
    BackgroundFinishedHint,
    BackgroundOutcomeDone,
    BackgroundOutcomeFailed,
    BackgroundOutcomeCancelled,
    BackgroundOutcomeKilled,
    BackgroundOutcomeTimedOut,
    BackgroundExitCode,
    BackgroundQuiet,
    BackgroundUsingTool,
    BackgroundStep,
    BackgroundFilesChanged,
    NotificationBackgroundStopped,
    NotificationFullResultPointer,
    // Footer chips.
    FooterWorkedChip,
    FooterPermissionKeyHint,
    ApprovalSummaryRunCommand,
    ApprovalSummaryRunShell,
    ApprovalSummaryShellWait,
    ApprovalSummaryShellInput,
    ApprovalSummaryShellStop,
    ApprovalSummaryWritePath,
    ApprovalSummaryWriteFile,
    ApprovalSummaryEditPath,
    ApprovalSummaryEditFile,
    ApprovalSummaryEditPathMore,
    ApprovalSummaryApplyPatch,
    ApprovalSummaryReadPath,
    ApprovalSummaryReadFile,
    ApprovalSummaryListPath,
    ApprovalSummaryListWorkspace,
    ApprovalSummaryFetchUrl,
    ApprovalSummaryFetchPage,
    ApprovalSummarySearchQuery,
    ApprovalSummarySearchWeb,
    ApprovalSummaryImageQuery,
    ApprovalSummaryOpenTarget,
    ApprovalSummaryMore,
    ApprovalSummaryFollowLink,
    ApprovalSummaryFindOnPage,
    ApprovalSummaryScreenshot,
    ApprovalSummaryBrowse,
    ApprovalSummaryMcpTool,
    ApprovalSummaryUseName,
    ApprovalSummaryUseTool,
    // Fleet setup wizard.
    FleetDraftTitle,
    FleetDraftHeader,
    // Remote setup on-ramp.
    SetupRemoteOnRampText,
    // Approval dialog — localized descriptions.
    ApprovalDescSafe,
    ApprovalDescFileWrite,
    ApprovalDescShell,
    ApprovalDescNetwork,
    ApprovalDescMcpRead,
    ApprovalDescMcpAction,
    ApprovalDescAgent,
    ApprovalDescUnknown,
    // Approval impact summaries.
    ApprovalImpactSafe,
    ApprovalImpactFileWrite,
    ApprovalImpactShell,
    ApprovalImpactNetwork,
    ApprovalImpactMcpRead,
    ApprovalImpactMcpAction,
    ApprovalImpactAgent,
    ApprovalImpactUnknown,
    // Approval detail labels.
    ApprovalLabelCommand,
    ApprovalLabelDir,
    ApprovalLabelFile,
    ApprovalLabelPreview,
    ApprovalLabelProposedContent,
    ApprovalLabelReplaceThis,
    ApprovalLabelWithThis,
    ApprovalLabelReplacementContent,
    ApprovalLabelPath,
    ApprovalLabelTarget,
    ApprovalLabelInput,
    ApprovalLabelAction,
    ApprovalLabelType,
    ApprovalLabelPrompt,
    // Approval header labels.
    ApprovalLabelAbout,
    ApprovalLabelImpact,
    // Setup wizard — constitution file state.
    SetupConstitutionFileNotChecked,
    SetupConstitutionFileMissing,
    SetupConstitutionFileLoadedSelected,
    SetupConstitutionFileLoadedInactive,
    SetupConstitutionFileLoadedUnselected,
    SetupConstitutionFileEmpty,
    SetupConstitutionFileInvalid,
    SetupConstitutionFileUnreadable,
    SetupConstitutionFilePathError,
    // Setup wizard — expert override state.
    SetupExpertOverrideNotChecked,
    SetupExpertOverrideMissing,
    SetupExpertOverrideActive,
    SetupExpertOverrideDisabled,
    SetupExpertOverrideEmpty,
    SetupExpertOverrideUnreadable,
    SetupExpertOverridePathError,
    // Setup wizard — autonomy fallback.
    SetupAutonomyUnspecified,
    // Setup wizard — purpose labels.
    SetupGuidedPurposeCoding,
    SetupGuidedPurposeResearch,
    SetupGuidedPurposeOperations,
    SetupGuidedPurposeMixed,
    // Setup wizard — purpose about descriptions.
    SetupGuidedPurposeAboutCoding,
    SetupGuidedPurposeAboutResearch,
    SetupGuidedPurposeAboutOperations,
    SetupGuidedPurposeAboutMixed,
    // Setup wizard — working style descriptions.
    SetupGuidedStyleCoding,
    SetupGuidedStyleResearch,
    SetupGuidedStyleOperations,
    SetupGuidedStyleMixed,
    // Setup wizard — evidence labels.
    SetupGuidedEvidenceAssumptions,
    SetupGuidedEvidenceTestsAndReceipts,
    SetupGuidedEvidenceReleaseReceipts,
    // Setup wizard — guided answer notes.
    SetupGuidedNotes,
    // Underwater launch screen (pre-session menu + worktree flow).
    LaunchStartTitle,
    /// Arming line for a clicked recent-work row. Resuming replaces the whole
    /// session context, so the click arms and a second activation confirms.
    LaunchResumeConfirm,
    LaunchResumeConfirmTitle,
    LaunchResumeConfirmBody,
    LaunchResumeConfirmResume,
    LaunchResumeConfirmCancel,
    LaunchWorkDescription,
    LaunchChatDescription,
    LaunchWorkspaceGitReady,
    LaunchWorkspaceFolderReady,
    LaunchProviderConfigured,
    LaunchProviderSetupNeeded,
    LaunchGroupContinue,
    LaunchGroupMore,
    LaunchWorkspaceGitShort,
    LaunchWorkspaceFolderShort,
    LaunchProviderConfiguredShort,
    LaunchProviderSetupShort,
    LaunchMenuChangelog,
    LaunchWorktreePrompt,
    LaunchWorktreeNeedsGit,
    LaunchWorktreeNameLabel,
    LaunchHintMove,
    LaunchHintOpen,
    LaunchTipFlags,
    LaunchSavedSessionSingular,
    LaunchSavedSessionsPlural,
    LaunchCreatingWorktree,
    LaunchWorktreeFailed,
    LaunchWorktreeCreated,
    LaunchNoSavedSessions,
    LaunchNewSession,
    LaunchRecentHeading,
    LaunchHelpLine,
    LaunchSeeAllSessions,
    LaunchNoRecentSessions,
    LaunchResumeFailed,
    LaunchNoModelConnected,
    DispatchNotSentNoModel,
    DispatchNotSentSaveKey,
    DispatchNotSentStatus,
    AuthRejectedRecovery,
    ProviderKeyRejected,
    ProviderKeyForbidden,
    ProviderKeyUnreachable,
    ProviderKeyCheckFailed,
    LocalModelAdopted,
    ProviderPickerNotReopened,
    LaunchRunCommand,
    LaunchMcpConnectedOne,
    LaunchMcpConnectedMany,
    LaunchMcpNeedsSignInOne,
    LaunchMcpNeedsSignInMany,
    LaunchMenuNewWorktree,
    LaunchMenuResume,
    LaunchMenuQuit,
    LaunchNoticeClaude,
    ReceiptSessionHooks,
    // Underwater shell phase words (footer status band).
    PhaseIdle,
    PhaseDraft,
    PhaseWorking,
    PhaseReasoning,
    PhaseReading,
    PhaseUsingTool,
    /// Live sub-users (agent spawns) are running — reading differently from
    /// tool work so the shell can signal orchestration.
    PhaseSubagents,
    /// Metered verification pass (tests/checks) — distinct from `working`
    /// so checking reads differently from searching (ocean state model).
    PhaseVerifying,
    PhaseWaitingOnYou,
    PhaseDone,
    PhaseFailed,
    PhaseFinishing,
    // Underwater header chips: mode and permission words.
    ChipModeAct,
    ChipModePlan,
    ChipModeOperate,
    ChipPermissionReadOnly,
    ChipPermissionAsk,
    ChipPermissionAuto,
    ChipPermissionFullAccess,
    ChipPermissionNever,
    // Underwater footer right-hand hint words (keys stay literal in code).
    FooterHintKeys,
    FooterHintOutput,
    FooterHintContext,
    InfoLineHelp,
    InfoLineContext,
    InfoLineTtft,
    InfoLinePeak,
    InfoLineOffPeak,
    InfoLineWhales,
    InfoLineAutomation,
    InfoLineNotConnected,
    InfoLineThinking,
    // Session metrics strip short labels (phase strip ledger and /status).
    SessionMetricsTurn,
    SessionMetricsTurns,
    SessionMetricsStep,
    SessionMetricsSteps,
    SessionMetricsLlm,
    SessionMetricsTools,
    SessionMetricsTtft,
    SessionMetricsTokensPerSecond,
    RuntimeStoreRecovered,
    ResumeExactSessionHint,
    ResumeSavedSessionHint,
    GoalProgressLabel,
    GoalProgressReceipt,
    GoalProgressNow,
    GoalProgressNext,
    CmdCacheUnpricedNote,
    SessionMetricsCache,
    SessionMetricsInput,
    SessionMetricsStatusLine,
    // `/status` report labels and runtime summaries.
    StatusLabelRoute,
    StatusLabelDirectory,
    StatusLabelProjectDocs,
    StatusLabelMode,
    StatusLabelSafety,
    StatusLabelMcp,
    StatusLabelFleet,
    StatusLabelContextWindow,
    StatusLabelWindowSource,
    StatusLabelWindowOverride,
    StatusLabelCatalog,
    StatusLabelCloudFacts,
    StatusLabelSession,
    StatusLabelSessionTokens,
    StatusLabelSessionCost,
    StatusLabelToolOutputs,
    StatusRouteSummary,
    StatusProjectDocsNone,
    StatusPostureSummary,
    StatusShellOn,
    StatusShellOff,
    StatusTrustedWorkspace,
    StatusWorkspace,
    StatusApprovalAsk,
    StatusApprovalAuto,
    StatusApprovalFullAccess,
    StatusApprovalNever,
    StatusMcpConfigured,
    StatusFleetDrifted,
    StatusModelNotInRoster,
    StatusContextUsage,
    StatusContextSourceConfigured,
    StatusContextSourceConfiguredModel,
    StatusContextSourceProviderReported,
    StatusContextSourceKimiSafeFloor,
    StatusContextSourceCatalog,
    StatusContextSourceModelHint,
    StatusContextSourceFallback,
    StatusWindowOverrideProvider,
    StatusWindowOverrideActiveProvider,
    StatusSessionNotSaved,
    StatusSessionSummary,
    StatusSessionTokensSummary,
    StatusCacheNotReported,
    StatusCacheSummary,
    StatusToolRawPressure,
    StatusToolCompactReceipts,
    StatusToolArtifacts,
    StatusToolNone,
    StatusSafetyReadOnlyUnenforced,
    StatusSafetyReadOnly,
    StatusSafetyWorkspaceWriteUnenforcedNetworkOn,
    StatusSafetyWorkspaceWriteUnenforcedNetworkOff,
    StatusSafetyWorkspaceWriteNetworkOn,
    StatusSafetyWorkspaceWriteNetworkOff,
    StatusSafetyDisabled,
    StatusSafetyDisabledSetuidBlocked,
    StatusSafetyDisabledSetuidAllowed,
    StatusSafetyExternal,
    StatusPointers,
    // Underwater post-launch empty state.
    EmptyStateNoGit,
    EmptyStateMcpLabel,
    EmptyStatePrompt,
    // Session picker surface.
    SessionsSurfaceTitle,
    SessionsPaneTitle,
    SessionsHistoryPaneTitle,
    SessionsActionResume,
    SessionsActionSearch,
    SessionsActionSort,
    SessionsActionRename,
    SessionsActionAllWorkspaces,
    SessionsActionDelete,
    SessionsActionClose,
    SessionsScopeSortHeader,
    SessionsEmptyTitle,
    SessionsEmptyHint,
    SessionsShowingAllWorkspaces,
    SessionsScopedToWorkspace,
    SessionsNewTitlePrompt,
    SessionsDeletePrompt,
    SessionsConfirmDelete,
    SessionsNewSessionTitle,
    SessionsOpenedHistory,
    SessionsSortStatus,
    SessionsSortRecent,
    SessionsSortName,
    SessionsSortSize,
    SessionsSearchPrompt,
    SessionsDeleteFailed,
    SessionsDeleteOpenHere,
    SessionsDeleteOpenElsewhere,
    SessionSaveFailed,
    SessionSaveFailedAtExit,
    SessionsDeleted,
    SessionsNoSelection,
    SessionsTitleLength,
    SessionsOpenFailed,
    SessionsLoadFailed,
    SessionsResumed,
    SessionsRenameFailed,
    SessionsRenamed,
    SessionsRailTitle,
    SessionsRailEmpty,
    SessionsRailBrowseAll,
    SessionsRailShowingCount,
    SessionsRailUnavailable,
    SessionsActionArchive,
    SessionsActionShowArchived,
    SessionsArchived,
    SessionsRestored,
    SessionsArchiveFailed,
    SessionsShowingArchived,
    SessionsHidingArchived,
    SessionsArchivedCompact,
    SessionsNoResults,
    SessionsDirectoryFailed,
    SessionsPreviewFailed,
    SessionsDeleteCancelled,
    SessionsRenameCancelled,
    SessionsShowingRange,
    SessionsMessageCountCompact,
    SessionsForkCompact,
    SessionsCurrentCompact,
    SessionsUnknownMode,
    SessionsPreviewTitle,
    SessionsPreviewId,
    SessionsPreviewUpdated,
    SessionsPreviewMessagesModel,
    SessionsPreviewMode,
    SessionsToolCall,
    SessionsToolError,
    SessionsToolResult,
    SessionsServerTool,
    SessionsImage,
    SessionsTimeJustNow,
    SessionsTimeMinutesAgo,
    SessionsTimeHoursAgo,
    SessionsTimeDaysAgo,
    // Compact context inspector (Alt+C surface).
    CtxInspRowSystemPrompt,
    CtxInspRowMessages,
    CtxInspRowFree,
    CtxInspFreeTokensDetail,
    CtxInspDrillTitle,
    CtxInspSurfaceTitle,
    CtxInspActionSelect,
    CtxInspActionDrillDown,
    CtxInspActionClose,
    CtxInspUsedTokens,
    CtxInspAutoCompactAt,
    CtxInspRowTokens,
    CtxInspRowCompaction,
    CtxInspRowAnchors,
    CtxInspCompactionNever,
    CtxInspCompactionDetail,
    CtxInspCompactionRestored,
    CtxInspCompactionPathSummary,
    CtxInspCompactionPathPrune,
    CtxInspCompactionAssistantKept,
    CtxInspAnchorsNone,
    CtxInspAnchorsPresent,
    // Model picker route surface.
    RouteSurfaceTitle,
    RouteBrowseCatalog,
    RouteActionType,
    RouteActionSearchAnyModel,
    RoutePanelHeader,
    RouteProviderLabel,
    RouteModelFirstAtomic,
    PickerActionMove,
    PickerActionSwitch,
    PickerActionApply,
    PickerActionAssignRoute,
    FleetRoutePickUnavailable,
    FleetRouteSaved,
    FleetRouteInherited,
    FleetRouteNotInCatalog,
    PickerActionSetStartupDefault,
    PickerActionPin,
    PickerActionFleet,
    PickerActionCancel,
    PickerActionClear,
    PickerActionClearSearch,
    PickerActionBrowseAll,
    PickerActionCustom,
    PickerActionJump,
    PickerActionEditKey,
    PickerActionModels,
    PickerActionUnavailable,
    PickerActionSetKey,
    PickerActionConfigured,
    RouteNoModels,
    RouteNoModelMatch,
    ProviderNoMatchesTitle,
    ProviderNoMatchesHint,
    ProviderNoConfiguredTitle,
    ProviderNoConfiguredHint,
    ProviderNoCatalogModels,
    // Provider picker — informed external-credential consent.
    ProviderExternalActionRevoke,
    ProviderExternalActionChoices,
    ProviderExternalActionReuseGrok,
    ProviderExternalHintCodexReview,
    ProviderExternalHintXaiReview,
    ProviderExternalHintXaiApiKey,
    XaiAuthChoiceTitle,
    XaiAuthChoiceIntro,
    XaiAuthChoiceApiKeyOption,
    XaiAuthChoiceDeviceOAuthOption,
    OrcarouterAuthChoiceTitle,
    OrcarouterAuthChoiceIntro,
    OrcarouterAuthChoiceApiKeyOption,
    OrcarouterAuthChoicePkceOption,
    ChatgptAuthChoiceTitle,
    ChatgptAuthChoiceIntro,
    ChatgptAuthChoicePkceOption,
    ChatgptAuthChoiceImportOption,
    ProviderExternalHintChatgptReview,
    ProviderExternalActionReuseCodex,
    ProviderExternalDetailScope,
    ProviderExternalDormant,
    ProviderExternalOwnerPath,
    ProviderExternalPinnedPathWarning,
    ToolProjectionWarning,
    SnapshotsDisabledTooLarge,
    SnapshotsDisabledTooManyFiles,
    SnapshotsDisabledUnsafeLocation,
    SnapshotsHistoryRepaired,
    SnapshotsFailing,
    SessionIdDivergedNotice,
    RuntimeStoreUnreadableNotice,
    RuntimeStoreUnwritableNotice,
    ProviderExternalSemanticsRevoke,
    ProviderExternalRevoke,
    ProviderExternalChoiceTitle,
    ProviderExternalActionChoose,
    ProviderExternalChoiceIntro,
    ProviderExternalDisabledLabel,
    ProviderExternalDisabledDetail,
    ProviderExternalReadOnlyLabel,
    ProviderExternalReadOnlyDetail,
    ProviderExternalReadOnlySemantics,
    ProviderExternalManagedLabel,
    ProviderExternalManagedDetail,
    ProviderExternalConfirmTitle,
    ProviderExternalActionGrant,
    ProviderExternalOwnerLabel,
    ProviderExternalExactPathLabel,
    ProviderExternalSemanticsLabel,
    ProviderExternalRejectUnsafe,
    ProviderExternalRevokeLabel,
    ProviderExternalRouteLabel,
    ProviderExternalCustodyLine,
    ProviderExternalBillingLine,
    ProviderExternalRevokeScope,
    ProviderExternalOwnerOnly,
    ProviderExternalPinnedPathChanged,
    ProviderExternalRevokeConfirmTitle,
    ProviderExternalGrantedToast,
    ProviderExternalSaveFailedToast,
    ProviderExternalRevokedToast,
    ProviderExternalRevokeFailedToast,
    // Theme picker surface.
    ThemeSurfaceTitle,
    // Fleet roster room.
    FleetRosterHeaderLabel,
    FleetRosterTabRoster,
    FleetRosterTabSetup,
    FleetRosterWorkers,
    FleetRosterMembersCount,
    FleetRosterOperatorFirst,
    FleetRosterOperatorRow,
    /// Roster row badge when a project file is winning the same id.
    FleetRosterShadowBadgeProjectOverride,
    /// Roster row badge when a personal file exists but is ignored.
    FleetRosterShadowBadgePersonalIgnored,
    /// Roster row badge when a personal file is winning the same id.
    FleetRosterShadowBadgePersonalOverride,
    /// Roster row badge when `[fleet.profiles]` is winning the same id.
    FleetRosterShadowBadgeConfigOverride,
    /// Detail-pane heading for the full per-id layer stack.
    FleetRosterLayersLabel,
    /// Marker on the winning layer in the detail stack.
    FleetRosterLayerWins,
    /// Marker on a displaced layer in the detail stack.
    FleetRosterLayerIgnored,
    FleetReadyNotice,
    /// Sticky error when Fleet profile save cannot prove collision safety.
    FleetProfileIdentityVerifyFailed,
    /// Sticky error when the drafted profile id collides with another file.
    FleetProfileIdConflict,
    /// Sticky error when the drafted profile pins an unconfigured provider.
    FleetProfileProviderUnconfigured,
    // The fleet as models: `/fleet models|add|remove`, the picker's ⇧F, and
    // the fleet lines of `/models` (design MODEL-ROUTING-CATALOG §10 F1).
    FleetModelAdded,
    FleetModelAddedAs,
    FleetModelAddedCreatedNote,
    FleetModelAddedSelectedNote,
    FleetModelRemoved,
    FleetModelRemovedRoles,
    FleetModelUnchanged,
    FleetModelReasonOperatorRoute,
    FleetModelReasonAlreadyPresent,
    FleetModelErrorNeedsRoute,
    FleetModelErrorNeedsRole,
    FleetReviewWorkspaceLimits,
    FleetReviewPolicy,
    FleetModelErrorNoSelection,
    FleetModelErrorOperatorRoute,
    FleetModelErrorNotInFleet,
    FleetModelsEmpty,
    FleetModelsHeader,
    FleetModelsBroken,
    FleetModelsFooter,
    FleetModelsFactPrice,
    FleetModelsFactContext,
    FleetModelsFactTools,
    FleetAddUsage,
    FleetAddProviderUnconfigured,
    FleetAddModelNotServed,
    FleetAddFailed,
    FleetRemoveUsage,
    FleetRemoveFailed,
    FleetToggleFailed,
    // Fleet setup destination step and review actions (save-scope redesign).
    FleetDestStepTitle,
    FleetDestStepSubtitle,
    FleetDestProjectLabel,
    FleetDestPersonalLabel,
    FleetDestProjectSummary,
    FleetDestPersonalSummary,
    FleetDestProjectDescription,
    FleetDestPersonalDescription,
    FleetDestPathLine,
    FleetDestUnavailable,
    FleetDestReasonNoProjectConfig,
    FleetDestReasonWorkspaceMissing,
    FleetDestReasonHomeUnavailable,
    FleetDestWillReplace,
    FleetDestOverridesProject,
    FleetDestOverridesPersonal,
    FleetDestOverridesBuiltIn,
    FleetSavesToChip,
    FleetSavesToUndecided,
    FleetActionSaveProject,
    FleetActionSavePersonal,
    FleetActionReplaceProject,
    FleetActionReplacePersonal,
    FleetActionConfirmReplace,
    FleetActionChangeDestination,
    FleetActionBack,
    FleetReviewSavesTo,
    FleetModelRowBlockedNotice,
    FleetDestProjectDisabledSave,
    // Workflow panel.
    WorkflowStatusWaiting,
    WorkflowStatusDegraded,
    WorkflowRunFailedToast,
    WorkflowDebrief,
    WorkflowDispatchFailureLine,
    WorkflowDispatchFailuresOmitted,
    WorkflowDispatchFallbackTask,
    WorkflowTranscriptDetails,
    WorkflowReceiptRole,
    WorkflowReceiptReasoning,
    WorkflowReceiptVia,
    WorkflowReceiptTokens,
    WorkflowReceiptTools,
    WorkflowReceiptDuration,
    WorkflowReceiptUnknown,
    WorkflowReceiptProviderReported,
    WorkflowReceiptEstimated,
    // Sidebar work strip.
    SidebarTasksLabel,
    TaskOwnershipUnverified,
    TaskInventoryUnavailable,
    SidebarTodoLabel,
    SidebarStopControl,
    SidebarDestructiveArmed,
    WorkSurfaceTodoProgress,
    WorkSurfaceStopConfirmHint,
    TerminalDockStarting,
    TerminalDockControls,
    TerminalDockEmpty,
    TerminalDockUnsupported,
    TerminalDockLost,
    TerminalDockDropped,
    TerminalDockError,
    TerminalDockPasteRefused,
    TerminalDockExited,
    TerminalDockLive,
    CoordinationWorkTitle,
    CoordinationSummaryDecisions,
    CoordinationSummaryContentions,
    CoordinationSummaryReconciled,
    CoordinationSchema,
    CoordinationSequence,
    CoordinationPerSectionLimit,
    CoordinationDecisionsHeading,
    CoordinationNone,
    CoordinationNoneValue,
    CoordinationStatus,
    CoordinationOwner,
    CoordinationVersion,
    CoordinationWriteClaimsHeading,
    CoordinationIsolated,
    CoordinationSharedWorkspace,
    CoordinationPaths,
    CoordinationContracts,
    CoordinationContentionsHeading,
    CoordinationClaimant,
    CoordinationDisposition,
    CoordinationNeutralReconciliationHeading,
    CoordinationCandidates,
    CoordinationRetry,
    CoordinationReviewer,
    CoordinationVerifier,
    CoordinationVerification,
    CoordinationContextProjectionsHeading,
    CoordinationContextDecisions,
    CoordinationBytes,
    CoordinationDeduplicated,
    CoordinationOmitted,
    CoordinationActiveHotPathsHeading,
    CoordinationActiveClaims,
    CoordinationMetricsNoteHeading,
    CoordinationMetricsNoAuthoritativeSource,
    CoordinationStatusProposed,
    CoordinationStatusAccepted,
    CoordinationStatusSuperseded,
    // Composer slash menu.
    ComposerSlashMenuHint,
    // Approval modal — repository law band.
    ApprovalRepoLawBadge,
    ApprovalRepoLawTitle,
    ApprovalRepoLawWarning,
    ApprovalRepoLawRuleLabel,
    // Fuzzy file picker (@ attach overlay).
    FilePickerMatchSingular,
    FilePickerMatchesPlural,
    FilePickerScanning,
    // Quiet action-triggered product guidance.
    BehavioralTipPlanning,
    BehavioralTipBackgroundReceipt,
    BehavioralTipClearedInput,
    BehavioralTipMcpValidation,
    BehavioralTipRepeatedCommand,
    BehavioralTipDurableStateWritten,
    BehavioralTipTodoWrite,
    ConfigLabelContextualTips,
    ConfigHintContextualTips,
    ContextualTipsNotSaved,
    // Live-route settings lock (#2982): refusals and startup-default receipts.
    SettingLockedDuringTurn,
    SettingSubjectMode,
    SettingSubjectThinking,
    SettingSubjectModel,
    SettingSubjectModelAndThinking,
    SettingSubjectProvider,
    SettingSubjectPermissions,
    ThinkingControlledByAutoRouting,
    SavedAsStartupDefault,
    ModeAlreadyActiveSavedAsDefault,
    StartupDefaultNotSaved,
    RouteSaveNothingPending,
    RouteSaveNoTeamSelected,
    RouteSaveTeamUpdated,
    RouteSaveTeamUpdateFailed,
    RouteSaveTeamUpdateFailedMoved,
    RouteSaveTeamCreateFailed,
    RouteSaveSelectedNote,
    RouteSaveSelectionFailedNote,
    RouteSaveSavedAsNewTeam,
    RouteSaveFailed,
    RouteSaveRouteNoLongerActive,
    RouteSaveRememberedDefault,
    StartupDefaultSubjectMode,
    StartupDefaultSubjectThinking,
    StartupDefaultSubjectModel,
    StartupDefaultSubjectAll,
    // Durable scheduled automation operator receipts.
    AutomationUsage,
    AutomationManagerUnavailable,
    AutomationListFailed,
    AutomationActionFailed,
    AutomationEmpty,
    AutomationListHeading,
    AutomationScopeNote,
    AutomationNoun,
    AutomationStatusLabel,
    AutomationStatusActive,
    AutomationStatusPaused,
    AutomationRunStatusQueued,
    AutomationRunStatusRunning,
    AutomationRunStatusCompleted,
    AutomationRunStatusFailed,
    AutomationRunStatusCanceled,
    AutomationActionInspect,
    AutomationActionPause,
    AutomationActionResume,
    AutomationActionDelete,
    AutomationActionRun,
    AutomationActionCancel,
    AutomationActionPaused,
    AutomationActionResumed,
    AutomationNextLabel,
    AutomationNameLabel,
    AutomationEditorNew,
    AutomationEditorEdit,
    AutomationEditorSchedule,
    AutomationEditorDaily,
    AutomationEditorWeekly,
    AutomationEditorHourly,
    AutomationEditorOnce,
    AutomationEditorCustom,
    AutomationEditorTime,
    AutomationEditorDate,
    AutomationEditorDays,
    AutomationEditorLocalTime,
    AutomationEditorProvider,
    AutomationEditorDefaultModel,
    AutomationEditorInheritedProvider,
    AutomationEditorEnabled,
    AutomationEditorControls,
    AutomationEditorPromptControls,
    AutomationEditorSaveFailed,
    AutomationEditorBusy,
    AutomationEditorSaved,
    AutomationEditorPreviewUnavailable,
    AutomationEditorConflict,
    AutomationEditorInvalidWorkspace,
    AutomationEditorModelControls,
    AutomationEditorPausedPreview,
    AutomationEditorInvalidSchedule,
    AutomationEditorMonday,
    AutomationEditorTuesday,
    AutomationEditorWednesday,
    AutomationEditorThursday,
    AutomationEditorFriday,
    AutomationEditorSaturday,
    AutomationEditorSunday,
    AutomationPromptLabel,
    AutomationCwdLabel,
    AutomationModeLabel,
    AutomationAllowShellLabel,
    AutomationTrustModeLabel,
    AutomationAutoApproveLabel,
    AutomationRruleLabel,
    AutomationDeliveryLabel,
    AutomationLastLabel,
    AutomationRecentRunsLabel,
    AutomationNoRuns,
    AutomationRunsUnavailable,
    AutomationTaskLabel,
    AutomationDeletePreview,
    AutomationDeleteConfirmationStale,
    // Activity-band slot and typed receipt cards (AUTOMATION-VISIBILITY §2).
    AutomationBandScheduled,
    AutomationReceiptFired,
    AutomationReceiptStarted,
    AutomationReceiptCompleted,
    AutomationReceiptCoalesced,
    AutomationReceiptMissed,
    AutomationReceiptExpired,
    AutomationReceiptDeleted,
    AutomationRunLabel,
    AutomationDeletedRunsDetail,
    /// Whale Teams state words, species, and jobs (crates/tui/src/tui/whales.rs).
    WhaleStateResting,
    WhaleStateThinking,
    WhaleStateWorking,
    WhaleStateWaiting,
    WhaleStateBlocked,
    WhaleStateOffline,
    /// Parked-child status word and its recovery line (#5906). A child parked
    /// at the parent's turn end is not "waiting for input": nothing will
    /// answer it, so the surfaces name the state and the two ways out.
    AgentStatusParked,
    AgentStatusParkedRecovery,
    WhaleAnimalScout,
    WhaleAnimalPatch,
    WhaleAnimalHarbor,
    WhaleAnimalEcho,
    WhaleAnimalKeel,
    WhaleAnimalLantern,
    WhaleAnimalPlain,
    WhaleJobScout,
    WhaleJobPatch,
    WhaleJobHarbor,
    WhaleJobEcho,
    WhaleJobKeel,
    WhaleJobLantern,
    WhaleJobPlain,
    AgentFocusOpened,
    AgentFocusClosed,
    AgentFocusBanner,
    AgentFocusPosture,
    AgentFocusPostureWrites,
    AgentFocusPostureReadOnly,
    AgentFocusPostureNetwork,
    AgentFocusPostureNoNetwork,
    AgentFocusPostureShellFull,
    AgentFocusPostureShellReadOnly,
    AgentFocusPostureShellNone,
    AgentFocusComposerChip,
    AgentFocusPlaceholder,
    AgentFocusNoTranscript,
    AgentFocusOmitted,
    AgentFocusResult,
    AgentFocusStopReason,
    AgentFocusFollowUpDelivered,
    AgentFocusFollowUpQueued,
    AgentFocusFollowUpContinued,
    AgentFocusFollowUpFailed,
    FooterHintForAgents,
    FooterHintToManage,
    AgentRailQueuedCount,
    WorkbarSoFar,
    WorkbarLargeWorkflow,
    WorkbarMoreRuns,
    WorkflowLineStarted,
    WorkflowLineFinished,
    WorkflowLineFinishedWithGaps,
    WorkflowLineFailed,
    WorkflowLineStopped,
    WorkflowLineAgents,
    PickerActionTestConnection,
    ProviderCustomFormBaseUrl,
    ProviderCustomFormModel,
    ProviderCustomFormHint,
    ProviderConnectionChecked,
    ProviderConnectionCheckedPickModel,
    ProviderTestConnectionNeedKey,
    ProviderTestConnectionFailed,
    ProviderTestConnectionNoEndpoint,
    ComposerPlaceholderFollowUp,
    ComposerPlaceholderSendNow,
    ComposerHintSendWithQueue,
    ComposerHintQueue,
    ComposerHintQueueWithCount,
    ComposerHintOfflineQueue,
    ComposerHintOfflineConnect,
    ComposerHintSendNow,
    ComposerHintSendIntoTurn,
    PendingSendingIntoTurnPrefix,
    PendingCouldNotSendIntoTurnPrefix,
    PendingEditingFollowUpPrefix,
    PendingQueuedOnePrefix,
    PendingQueuedManyPrefix,
    PendingSendNowControls,
    PendingSendNowDropControls,
    PendingEscRestore,
    PendingQueuedFollowUpPrefix,
    PendingInputsHeader,
    PendingContextHeader,
    ToastQueuedFollowUp,
    ToastQueuedFollowUpCount,
    ToastQueuedOffline,
    ToastSentIntoTurn,
    ToastCouldNotSendIntoTurn,
    ToastHookBlockedFollowUp,
    ToastOfflineQueuedCount,
    // Operate plan board chrome (contract tokens stay untranslated).
    OperateBoardHeader,
    OperateBoardBurnObserved,
    OperateBoardBurnNoCap,
    OperateBoardDirectionEmpty,
    OperateBoardDirectionLine,
    OperateBoardPlanMissing,
    OperateBoardPlanHeader,
    OperateBoardGantt,
    // Tideline settings shell: categories, detail facts, sources, apply
    // semantics, editor kinds, and navigation copy.
    ConfigCategoryAppearance,
    ConfigCategoryModelsProviders,
    ConfigCategoryFleet,
    ConfigCategoryWork,
    ConfigCategoryToolsMcp,
    ConfigCategoryTrust,
    ConfigCategoryMotion,
    ConfigCategoryAdvanced,
    ConfigFactCurrent,
    ConfigFactSaved,
    ConfigFactStartup,
    ConfigFactSource,
    ConfigFactScope,
    ConfigFactApply,
    ConfigFactKind,
    ConfigFactAvailable,
    ConfigFactObserved,
    ConfigFactOpens,
    ConfigLaneUnobserved,
    ConfigSourceSession,
    ConfigSourceUserSettings,
    ConfigSourceConfig,
    ConfigSourceManaged,
    ConfigApplyEffectiveNow,
    ConfigApplyOnSave,
    ConfigApplyNextSession,
    ConfigApplyRestart,
    ConfigApplyReadOnly,
    ConfigApplyReload,
    ConfigApplyUiNowEngineRestart,
    ConfigKindToggle,
    ConfigKindChoice,
    ConfigKindNumber,
    ConfigKindText,
    ConfigKindAction,
    ConfigKindReadOnly,
    ConfigRowActionNote,
    ConfigRowDiagnosticNote,
    ConfigNavHint,
    ConfigActivateAgain,
    ConfigSearchLabel,
    ConfigEditChooseLabel,
    ConfigChoiceFooter,
    ConfigChoiceFooterCompact,
    ConfigEditorApply,
    ConfigEditorCancel,
    ConfigLaneUnavailable,
    ConfigSourceEnvironment,
    ConfigSourceTerminal,
    ConfigDescriptionDefault,
    ConfigValueOn,
    ConfigValueOff,
    ConfigValueProviderDefault,
    ConfigChoiceAsk,
    ConfigChoiceAutoReview,
    ConfigChoiceUseTuiDefault,
    ConfigChoiceFullAccess,
    ConfigChoiceNever,
    ConfigChoiceModeAct,
    ConfigChoiceModePlan,
    ConfigChoiceModeOperate,
    ConfigChoicePlacementTop,
    ConfigChoicePlacementBottom,
    ConfigChoicePlacementLeft,
    ConfigChoicePlacementRight,
    ConfigChoiceRailTasks,
    ConfigChoiceRailAgents,
    ConfigChoiceRailContext,
    ConfigChoiceStatusCw,
    ConfigChoiceStatusDots,
    ConfigChoiceDiffFull,
    ConfigChoiceDiffSummary,
    ConfigChoiceDetailAsk,
    ConfigChoiceDetailAutoReview,
    ConfigChoiceDetailUseTuiDefault,
    ConfigChoiceDetailFullAccess,
    ConfigChoiceDetailNever,
    ConfigChoiceDetailModeAgent,
    ConfigChoiceDetailModePlan,
    ConfigChoiceDetailModeOperate,
    ConfigChoiceDetailPlacementTop,
    ConfigChoiceDetailPlacementBottom,
    ConfigChoiceDetailPlacementLeft,
    ConfigChoiceDetailPlacementRight,
    ConfigChoiceDetailPlacementOff,
    ConfigChoiceDetailRailTasks,
    ConfigChoiceDetailRailAgents,
    ConfigChoiceDetailRailContext,
    ConfigChoiceDetailLowMotionOn,
    ConfigChoiceDetailLowMotionOff,
    ConfigChoiceDetailFancyOn,
    ConfigChoiceDetailFancyOff,
    ConfigChoiceDetailShowThinkingOn,
    ConfigChoiceDetailShowThinkingOff,
    ConfigChoiceDetailThinkingHighlightOn,
    ConfigChoiceDetailThinkingHighlightOff,
    ConfigHintModel,
    ConfigHintFastModel,
    ConfigHintProvider,
    ConfigHintApprovalMode,
    ConfigHintPermissionPosture,
    ConfigHintApprovalPolicy,
    ConfigHintManagedApprovalPolicy,
    ConfigHintManagedAllowShell,
    ConfigHintAllowShell,
    ConfigHintComposerMultilineMode,
    ConfigHintBooleanValues,
    ConfigHintDensity,
    ConfigHintInlineDiffs,
    ConfigHintToolCollapse,
    ConfigHintBackgroundColor,
    ConfigHintWorkSurfacePlacement,
    ConfigHintRailPanel,
    ConfigHintWorkSurfaceTopHeight,
    ConfigHintWorkSurfaceSideWidth,
    ConfigHintBaseUrl,
    ConfigHintContextWindow,
    ConfigHintEffectiveContextWindow,
    ConfigHintCostCurrency,
    ConfigHintCalmMode,
    ConfigHintLowMotion,
    ConfigHintFancyAnimations,
    ConfigHintShowThinking,
    ConfigHintThinkingDefaultExpanded,
    ConfigHintThinkingPreviewLines,
    ConfigHintHelpExpandGroups,
    ConfigHintPinLastPrompt,
    ConfigHintThinkingHighlight,
    ConfigHintSynchronizedOutput,
    ConfigHintDefaultMode,
    ConfigHintMaxHistory,
    ConfigHintAutoCompactThreshold,
    ConfigHintDefaultModel,
    ConfigHintReasoningEffort,
    ConfigHintMcpOpen,
    ConfigHintMcpReconnect,
    ConfigHintMcpDiagnose,
    ConfigHintPluginsOpen,
    ConfigHintMcpConfigPath,
    ConfigHintFleetMaxSpawnDepth,
    ConfigHintFeatureSubagents,
    ConfigHintFeatureWebSearch,
    ConfigHintFeatureApplyPatch,
    ConfigHintFeatureMcp,
    ConfigHintFeatureExecPolicy,
    ConfigHintFeatureVisionModel,
    ConfigHintGoalCommand,
    ConfigHintWorkflow,
    SelectionCopiedAsMarkdown,
    McpShowCachedWhileTurnRuns,
    McpShowUnavailableWhileTurnRuns,
    McpLivePoolRefreshDeferredWhileTurnRuns,
    McpRetryDeferredWhileTurnRuns,
    McpRetryStarted,
    McpRetryConnected,
    McpRetryNeedsLogin,
    McpRetryFailed,
    WorkflowCountRunning,
    WorkflowCountDone,
    WorkflowCountFailed,
    WorkflowCountCancelled,
    WorkflowNoTasksYet,
    WorkflowSettledOfTotal,
    WorkflowPhaseCountOne,
    WorkflowPhaseCountMany,
    WorkflowPhaseOrdinal,
    ModelPickerPinned,
    ModelPickerUnpinned,
    ModelPickerPinFailed,
    ModelPickerPinOrderUpdated,
    ModelPickerPinReorderFailed,
    ModelPickerReadinessRefreshed,
    ModelPickerOpenToRefresh,
    ModelPickerPinnedChip,
    AuthSignedInAs,
    AuthSignedInWithoutEmail,
    AuthReplacedPreviousSignInAs,
    AuthReplacedPreviousSignIn,
    AuthSameAccountAsBefore,
    AuthEnvTokenOutranksSignIn,
    /// Hover label for the pinned user-prompt header above the transcript:
    /// clicking the header jumps to the user message it names.
    PinnedPromptJumpToMessage,
    /// Plan hand-off: the question asked when a Plan turn ends with open
    /// To-do steps, its three choices, and the message sent on "Work".
    PlanHandoffHeader,
    PlanHandoffQuestion,
    PlanHandoffWorkAsk,
    PlanHandoffWorkAskDetail,
    PlanHandoffWorkAuto,
    PlanHandoffWorkAutoDetail,
    PlanHandoffKeepPlanning,
    PlanHandoffKeepPlanningDetail,
    PluginOAuthBrowser,
    PluginOAuthReady,
    PluginOAuthLocalLogout,
    PlanHandoffProceed,
}

#[allow(dead_code)]
pub const ALL_MESSAGE_IDS: &[MessageId] = &[
    MessageId::MobileStreamReplayFailed,
    MessageId::MobileStreamCatchUpFailed,
    MessageId::MobileStreamRuntimeShutdown,
    MessageId::MobileStreamEnded,
    MessageId::MobileStreamClosed,
    MessageId::MobileStreamReconnecting,
    MessageId::MobileStreamConnected,
    MessageId::ConversationChangedBeforeUndo,
    MessageId::SessionArchiveExported,
    MessageId::SessionArchiveSizes,
    MessageId::SessionArchiveNoArtifacts,
    MessageId::SessionArchiveRestoreHint,
    MessageId::CostReasonNotMoney,
    MessageId::CostReasonBillingUnknown,
    MessageId::CostReasonEndpointUnknown,
    MessageId::CostReasonRateMissing,
    MessageId::CostReasonLiveUnverified,
    MessageId::CostReasonRetiredAlias,
    MessageId::CostReasonTierMissing,
    MessageId::CostReasonRoutingDependent,
    MessageId::CostReasonCoverageMissing,
    MessageId::CostReasonTokenRateMissing,
    MessageId::CostReasonInvalidRate,
    MessageId::CostReasonCurrencyMissing,
    MessageId::CostReasonUsageConflict,
    MessageId::CostChipUnknown,
    MessageId::CostChipSubtotal,
    MessageId::CostChipSavedSubtotal,
    MessageId::CostChipLocal,
    MessageId::CostChipAllowance,
    MessageId::CostChipAllowancePercent,
    MessageId::McpLoginInProgress,
    MessageId::McpLoginStarting,
    MessageId::McpLoginBrowser,
    MessageId::McpLoginStored,
    MessageId::McpLoginFailed,
    MessageId::McpReloadAlreadyRunning,
    MessageId::McpLoginCancelled,
    MessageId::McpLoginHandshakeTimeout,
    MessageId::McpLoginServerNotFound,
    MessageId::McpDiagnosisUnobserved,
    MessageId::McpDiagnosisSummary,
    MessageId::McpDiagnosisLastError,
    MessageId::McpDiagnosisNext,
    MessageId::McpStateDisabled,
    MessageId::McpStateAuthorizationRequired,
    MessageId::McpStateConnecting,
    MessageId::McpStateFailed,
    MessageId::McpStateDisconnected,
    MessageId::ComposerPlaceholder,
    MessageId::ComposerDispatchFailedRestored,
    MessageId::DispatchFailedQueued,
    MessageId::DispatchFailedInitial,
    MessageId::HistorySearchPlaceholder,
    MessageId::HistorySearchTitle,
    MessageId::HistoryHintMove,
    MessageId::HistoryHintAccept,
    MessageId::HistoryHintRestore,
    MessageId::HistoryNoMatches,
    MessageId::TranscriptReasoningExpand,
    MessageId::TranscriptThought,
    MessageId::TranscriptThoughtFor,
    MessageId::ScreenModeFullscreenNotice,
    MessageId::ScreenModeInlineNotice,
    MessageId::ScreenModeMouseCaptureOn,
    MessageId::ScreenModeMouseCaptureOff,
    MessageId::ScreenModeUnchanged,
    MessageId::ImageInputRejectedResent,
    MessageId::ImageInputOmitted,
    MessageId::ProviderToolCallMissing,
    MessageId::TelemetryNoticeDefaultOn,
    MessageId::TelemetryNoticeHeadline,
    MessageId::TelemetryNoticeBody,
    MessageId::TelemetryNoticeCompactBody,
    MessageId::TelemetryNoticeChoiceKeep,
    MessageId::TelemetryNoticeChoiceDisable,
    MessageId::TelemetryNoticeActionChoose,
    MessageId::TelemetryNoticeActionConfirm,
    MessageId::TelemetryNoticeActionExit,
    MessageId::TelemetryNoticeReceiptEnabled,
    MessageId::TelemetryNoticeReceiptDisabled,
    MessageId::TelemetryNoticeReceiptEnabledUnsaved,
    MessageId::TelemetryNoticeReceiptDisabledUnsaved,
    MessageId::TelemetryPreferenceEnabledNextLaunch,
    MessageId::TelemetryPreferenceDisabled,
    MessageId::TelemetryPreferenceDisabledWithWarning,
    MessageId::TelemetryPreferenceDisabledForSession,
    MessageId::TelemetryPreferenceSaveFailed,
    MessageId::StatusPickerTitle,
    MessageId::StatusPickerInstruction,
    MessageId::StatusPickerActionToggle,
    MessageId::StatusPickerActionAll,
    MessageId::StatusPickerActionNone,
    MessageId::StatusPickerActionSave,
    MessageId::StatusPickerActionCancel,
    MessageId::HotbarSetupTitle,
    MessageId::HotbarSetupSourceApp,
    MessageId::HotbarSetupSourceSlash,
    MessageId::HotbarSetupSourceMcp,
    MessageId::HotbarSetupSourceSkill,
    MessageId::HotbarSetupSourcePlugin,
    MessageId::HotbarSetupStatusDisabled,
    MessageId::HotbarSetupStatusPrefill,
    MessageId::HotbarSetupStatusReady,
    MessageId::HotbarSetupDirtyModified,
    MessageId::HotbarSetupDirtyClean,
    MessageId::HotbarSetupNoAction,
    MessageId::HotbarSetupConfirmDisable,
    MessageId::HotbarSetupStatusLine,
    MessageId::HotbarSetupSlotOutOfRange,
    MessageId::HotbarSetupNoActionSelected,
    MessageId::HotbarSetupCannotAssign,
    MessageId::HotbarSetupNoActions,
    MessageId::HotbarSetupRecommended,
    MessageId::HotbarSetupEmptySlot,
    MessageId::HotbarSetupHelp,
    MessageId::HotbarActionVoiceToggleName,
    MessageId::HotbarActionVoiceToggleDescription,
    MessageId::HotbarActionSessionCompactName,
    MessageId::HotbarActionSessionCompactDescription,
    MessageId::HotbarActionModePlanName,
    MessageId::HotbarActionModePlanDescription,
    MessageId::HotbarActionModeAgentName,
    MessageId::HotbarActionModeAgentDescription,
    MessageId::HotbarActionModeYoloName,
    MessageId::HotbarActionModeYoloDescription,
    MessageId::HotbarActionModeOperateName,
    MessageId::HotbarActionModeOperateDescription,
    MessageId::HotbarActionReasoningCycleName,
    MessageId::HotbarActionReasoningCycleDescription,
    MessageId::HotbarActionReasoningCycleAutoDisabled,
    MessageId::HotbarActionSidebarToggleName,
    MessageId::HotbarActionSidebarToggleDescription,
    MessageId::HotbarActionFileTreeToggleName,
    MessageId::HotbarActionFileTreeToggleDescription,
    MessageId::HotbarActionPaletteOpenName,
    MessageId::HotbarActionPaletteOpenDescription,
    MessageId::HotbarActionTrustToggleName,
    MessageId::HotbarActionTrustToggleDescription,
    MessageId::CommandPaletteTitle,
    MessageId::CommandPaletteSubtitle,
    MessageId::ConfigTitle,
    MessageId::ConfigPreviewLabel,
    MessageId::ConfigHintExternalCredentials,
    MessageId::ConfigSubtitle,
    MessageId::ConfigModalTitle,
    MessageId::ConfigSearchPlaceholder,
    MessageId::ConfigNoSettings,
    MessageId::ConfigNoMatchesPrefix,
    MessageId::ConfigFilteredSettings,
    MessageId::ConfigShowing,
    MessageId::ConfigFooterDefault,
    MessageId::ConfigFooterScrollable,
    MessageId::ConfigFooterFiltered,
    MessageId::ConfigSectionProvider,
    MessageId::ConfigSectionModel,
    MessageId::ConfigSectionPermissions,
    MessageId::ConfigSectionNetwork,
    MessageId::ConfigSectionDisplay,
    MessageId::ConfigSectionComposer,
    MessageId::ConfigSectionSidebar,
    MessageId::ConfigSectionHistory,
    MessageId::ConfigSectionMcp,
    MessageId::ConfigSectionFleet,
    MessageId::ConfigSectionWorkflow,
    MessageId::ConfigSectionSession,
    MessageId::ConfigSectionLegacy,
    MessageId::ConfigSectionExperimental,
    MessageId::ConfigScopeSession,
    MessageId::ConfigScopeSaved,
    MessageId::ConfigCommandSource,
    MessageId::ConfigCommandInvalidValue,
    MessageId::ConfigSearchUpdated,
    MessageId::ConfigPromptSuggestionUpdated,
    MessageId::ConfigLabelNotificationQuiet,
    MessageId::ConfigLabelNotificationSound,
    MessageId::ConfigLabelNotificationCondition,
    MessageId::ConfigLabelNotificationMethod,
    MessageId::ConfigLabelNotificationThreshold,
    MessageId::ConfigLabelNotificationSummary,
    MessageId::ConfigLabelNotificationSubagents,
    MessageId::ConfigLabelNotificationTurnComplete,
    MessageId::ConfigLabelNotificationSubagentTerminal,
    MessageId::ConfigLabelNotificationApprovalNeeded,
    MessageId::ConfigLabelNotificationInputNeeded,
    MessageId::ConfigLabelNotificationElevationNeeded,
    MessageId::ConfigLabelNotificationModelNotify,
    MessageId::ConfigLabelNotificationCompletionSound,
    MessageId::ConfigLabelNotificationSoundFile,
    MessageId::ConfigLabelNotificationEventSoundEnabled,
    MessageId::ConfigLabelNotificationEventSoundEvents,
    MessageId::ConfigLabelNotificationEventSoundInterval,
    MessageId::ConfigLabelNotificationEventSoundQuiet,
    MessageId::ConfigHintNotificationPolicy,
    MessageId::ConfigHintNotificationSound,
    MessageId::ConfigHintNotificationLegacy,
    MessageId::ConfigChoiceNotificationWhale,
    MessageId::ConfigChoiceNotificationLegacy,
    MessageId::ConfigChoiceNotificationAlways,
    MessageId::ConfigChoiceNotificationUnfocused,
    MessageId::ConfigChoiceNotificationNever,
    MessageId::ConfigChoiceNotificationFile,
    MessageId::ConfigChoiceNotificationBell,
    MessageId::ConfigNotificationsSetHint,
    MessageId::ConfigNotificationUpdated,
    MessageId::ConfigNotificationsWholeNumber,
    MessageId::ConfigAuditSearchProvider,
    MessageId::ConfigAuditPromptSuggestion,
    MessageId::ConfigAuditNotifications,
    MessageId::ConfigHelpDiscoverable,
    MessageId::ConfigEditCancelled,
    MessageId::ConfigEditTitlePrefix,
    MessageId::ConfigEditScopeLabel,
    MessageId::ConfigEditCurrentLabel,
    MessageId::ConfigEditHintLabel,
    MessageId::ConfigEditNewLabel,
    MessageId::ConfigEditFooter,
    MessageId::ConfigLocalePartialBadge,
    MessageId::ConfigLocalePartialDetail,
    MessageId::ConfigRowEffective,
    MessageId::ConfigDefaultValue,
    MessageId::ConfigDefaultReasoning,
    MessageId::ConfigUnavailable,
    MessageId::ConfigLabelProvider,
    MessageId::ConfigLabelBaseUrlDeepseek,
    MessageId::ConfigLabelProviderUrl,
    MessageId::ConfigHintProviderUrl,
    MessageId::ConfigLabelModel,
    MessageId::ConfigLabelFastModel,
    MessageId::ConfigLabelDefaultModel,
    MessageId::ConfigLabelReasoningEffort,
    MessageId::ConfigLabelApprovalMode,
    MessageId::ConfigLabelPermissionPosture,
    MessageId::ConfigLabelApprovalPolicy,
    MessageId::ConfigLabelManagedApprovalPolicy,
    MessageId::ConfigLabelDefaultMode,
    MessageId::ConfigLabelAllowShell,
    MessageId::ConfigLabelManagedAllowShell,
    MessageId::ConfigLabelTelemetry,
    MessageId::ConfigHintTelemetry,
    MessageId::ConfigValueTelemetryOn,
    MessageId::ConfigValueTelemetryOff,
    MessageId::ConfigLabelStreamTimeout,
    MessageId::ConfigLabelTheme,
    MessageId::ConfigLabelLocale,
    MessageId::ConfigLabelBackground,
    MessageId::ConfigLabelWorkSurfacePlacement,
    MessageId::ConfigLabelTopHeight,
    MessageId::ConfigLabelSideWidth,
    MessageId::ConfigLabelCalmMode,
    MessageId::ConfigLabelLowMotion,
    MessageId::ConfigLabelFancyAnimations,
    MessageId::ConfigLabelShowThinking,
    MessageId::ConfigLabelThinkingHighlight,
    MessageId::ConfigLabelShowToolDetails,
    MessageId::ConfigLabelInlineDiffs,
    MessageId::ConfigLabelStatusIndicator,
    MessageId::ConfigLabelSynchronizedOutput,
    MessageId::ConfigLabelCostCurrency,
    MessageId::ConfigLabelTranscriptSpacing,
    MessageId::ConfigLabelToolCollapse,
    MessageId::ConfigLabelComposerDensity,
    MessageId::ConfigLabelComposerBorder,
    MessageId::ConfigLabelComposerMultilineMode,
    MessageId::ConfigLabelComposerVimMode,
    MessageId::ConfigLabelBracketedPaste,
    MessageId::ConfigLabelPasteBurstDetection,
    MessageId::ConfigLabelMentionMenuLimit,
    MessageId::ConfigLabelMentionMenuBehavior,
    MessageId::ConfigLabelMentionWalkDepth,
    MessageId::ConfigLabelWorkspaceFollowSymlinks,
    MessageId::ConfigLabelContextPanel,
    MessageId::ConfigLabelSessionsRail,
    MessageId::ConfigLabelSessionAutoResume,
    MessageId::ConfigLabelAutoCompact,
    MessageId::ConfigLabelAutoCompactThreshold,
    MessageId::ConfigLabelMaxHistory,
    MessageId::ConfigLabelMcpOpen,
    MessageId::ConfigLabelMcpReconnect,
    MessageId::ConfigLabelMcpDiagnose,
    MessageId::ConfigLabelPluginsOpen,
    MessageId::ConfigLabelMcpConfigPath,
    MessageId::ConfigLabelFleetSpawnDepth,
    MessageId::ConfigLabelGoalCommand,
    MessageId::ConfigLabelWorkflow,
    MessageId::ConfigLabelFeaturePrefix,
    MessageId::ConfigColumnSetting,
    MessageId::ConfigColumnValue,
    MessageId::ConfigColumnScope,
    MessageId::ConfigActionOpenProvider,
    MessageId::ConfigActionOpenModel,
    MessageId::ConfigActionOpenMcp,
    MessageId::ConfigActionMcpReconnect,
    MessageId::ConfigActionMcpDiagnose,
    MessageId::ConfigActionOpenPlugins,
    MessageId::ConfigActionToggle,
    MessageId::ConfigActionChoose,
    MessageId::ConfigActionEdit,
    MessageId::ConfigActionReadOnly,
    MessageId::ModelPickerAutoNetworkHint,
    MessageId::ModelPickerAutoNetworkActiveProviderHint,
    MessageId::ModelPickerAutoLocalHint,
    MessageId::ModelPickerAutoLastRoute,
    MessageId::AutoRouteSelectedToast,
    MessageId::HelpGroupCommonCommands,
    MessageId::HelpGroupAllCommands,
    MessageId::HelpTitle,
    MessageId::HelpSubtitle,
    MessageId::HelpFilterPlaceholder,
    MessageId::HelpFilterPrefix,
    MessageId::HelpNoMatches,
    MessageId::HelpSlashCommands,
    MessageId::HelpKeybindings,
    MessageId::HelpUserCommands,
    MessageId::ModelPickerNotListed,
    MessageId::HelpSkills,
    MessageId::HelpFooterTypeFilter,
    MessageId::HelpFooterMove,
    MessageId::HelpFooterJump,
    MessageId::HelpFooterClose,
    MessageId::CmdAnchorDescription,
    MessageId::CmdAttachDescription,
    MessageId::CmdBalanceDescription,
    MessageId::CmdImportClaudeDescription,
    MessageId::CmdCacheDescription,
    MessageId::CmdPreviewRequestDescription,
    MessageId::CmdToolsDescription,
    MessageId::CmdEffortDescription,
    MessageId::CmdTurnInspectDescription,
    MessageId::CmdClearDescription,
    MessageId::CmdCompactDescription,
    MessageId::CmdPurgeDescription,
    MessageId::CmdConfigDescription,
    MessageId::CmdPermissionsDescription,
    MessageId::PermissionsListHeader,
    MessageId::PermissionsNoRules,
    MessageId::PermissionsFileMissing,
    MessageId::PermissionsFileEmpty,
    MessageId::PermissionsFilePresent,
    MessageId::PermissionsRuleEntry,
    MessageId::PermissionsMatchExactCommand,
    MessageId::PermissionsMatchCommandPrefix,
    MessageId::PermissionsMatchExactPath,
    MessageId::PermissionsMatchAnyInvocation,
    MessageId::PermissionsScopeGlobal,
    MessageId::PermissionsScopeRepo,
    MessageId::PermissionsAppliesHere,
    MessageId::PermissionsInactiveHere,
    MessageId::PermissionsRemovePreview,
    MessageId::PermissionsRemoved,
    MessageId::PermissionsUsage,
    MessageId::PermissionsRuleNotFound,
    MessageId::AutoReviewReceiptGuardianAllowed,
    MessageId::AutoReviewReceiptGuardianDenied,
    MessageId::AutoReviewReceiptGuardianUnavailable,
    MessageId::AutoReviewReceiptDeterministicBlocked,
    MessageId::AutoReviewReceiptHeld,
    MessageId::FooterHintEscInterrupt,
    MessageId::PostureHintEnterAgain,
    MessageId::PermissionsPostureHeader,
    MessageId::PermissionsPostureAsk,
    MessageId::PermissionsPostureAuto,
    MessageId::PermissionsPostureBypass,
    MessageId::PermissionsPostureNever,
    MessageId::PermissionsReceiptsNote,
    MessageId::PermissionsOperationFailed,
    MessageId::CmdAuthDescription,
    MessageId::CmdConstitutionDescription,
    MessageId::CmdContextDescription,
    MessageId::CmdCostDescription,
    MessageId::CmdReceiptsDescription,
    MessageId::CmdDiffDescription,
    MessageId::CmdEditDescription,
    MessageId::CmdExitDescription,
    MessageId::CmdExportDescription,
    MessageId::CmdCopyDescription,
    MessageId::CmdCopyNoOutput,
    MessageId::CmdCopySuccess,
    MessageId::CmdCopySuccessNoBackup,
    MessageId::CmdCopyQueued,
    MessageId::CmdCopyQueuedNoBackup,
    MessageId::CmdCopyFailed,
    MessageId::CmdCopyFailedNoBackup,
    MessageId::CmdFeedbackDescription,
    MessageId::FeedbackNoSession,
    MessageId::FeedbackUnavailable,
    MessageId::FeedbackReviewNotice,
    MessageId::FeedbackHelp,
    MessageId::FeedbackDraftRequested,
    MessageId::CmdForkDescription,
    MessageId::CmdTreeDescription,
    MessageId::CmdBranchDescription,
    MessageId::CmdResumeDescription,
    MessageId::CmdGoalDescription,
    MessageId::GoalReceiptSet,
    MessageId::GoalReceiptSetOperate,
    MessageId::GoalControlAccepted,
    MessageId::GoalControlRuntimeUnavailable,
    MessageId::GoalStatusIdleHint,
    MessageId::GoalContinuationWaiting,
    MessageId::GoalContinuationReady,
    MessageId::GoalContinuationStopped,
    MessageId::CmdThemeDescription,
    MessageId::CmdHfDescription,
    MessageId::CmdHelpDescription,
    MessageId::CmdProfileDescription,
    MessageId::ProfileUsage,
    MessageId::ProfileSwitching,
    MessageId::ProfileSwitched,
    MessageId::ProfileStatus,
    MessageId::ProfileSwitchFailed,
    MessageId::CmdHomeDescription,
    MessageId::CmdOverviewDescription,
    MessageId::HomeBackToConversation,
    MessageId::HomeNavigationBusy,
    MessageId::CmdHooksDescription,
    MessageId::CmdAgentDescription,
    MessageId::CmdInitDescription,
    MessageId::CmdJobsDescription,
    MessageId::CmdDispatchDescription,
    MessageId::CmdLinksDescription,
    MessageId::CmdLoadDescription,
    MessageId::CmdLogoutDescription,
    MessageId::CmdLoginDescription,
    MessageId::CmdMcpDescription,
    MessageId::McpRecommendedUnknownId,
    MessageId::McpRecommendationsHeading,
    MessageId::McpRecommendationsSafety,
    MessageId::McpRecommendationGithub,
    MessageId::McpRecommendationChrome,
    MessageId::McpRecommendationPlaywright,
    MessageId::McpRecommendationContainerUse,
    MessageId::McpCapabilitiesAdvertised,
    MessageId::McpCapabilitiesLegacyFallback,
    MessageId::McpCapabilitiesNotObserved,
    MessageId::CmdPluginDescription,
    MessageId::ExtensionsActionAdd,
    MessageId::ExtensionsActionEnable,
    MessageId::ExtensionsActionDisable,
    MessageId::ExtensionsActionReview,
    MessageId::ExtensionsActionRemove,
    MessageId::ExtensionsStateOn,
    MessageId::ExtensionsStateOff,
    MessageId::ExtensionsStateNeedsReview,
    MessageId::ExtensionsReviewExplainer,
    MessageId::ExtensionsScopeGlobal,
    MessageId::ExtensionsScopeProject,
    MessageId::ExtensionsScopePlugin,
    MessageId::ExtensionsRemoveArmed,
    MessageId::ExtensionsActionEdit,
    MessageId::ExtensionsHooksAddLabel,
    MessageId::ExtensionsHooksAddDescription,
    MessageId::ExtensionsActionReload,
    MessageId::ExtensionsActionConnect,
    MessageId::ExtensionsActionReconnect,
    MessageId::ExtensionsActionReauth,
    MessageId::ExtensionsActionDiagnose,
    MessageId::ExtensionsActionManage,
    MessageId::ExtensionsActionFocus,
    MessageId::ExtensionsActionFold,
    MessageId::ExtensionsActionTabs,
    MessageId::ExtensionsCompatibilityFull,
    MessageId::ExtensionsCompatibilityPartial,
    MessageId::ExtensionsGroupBuiltIn,
    MessageId::ExtensionsGroupConfigured,
    MessageId::ExtensionsGroupProblems,
    MessageId::ExtensionsGroupRecommended,
    MessageId::ExtensionsGroupServers,
    MessageId::ExtensionsGroupNeedsAttention,
    MessageId::ExtensionsGroupNeedsLogin,
    MessageId::ExtensionsGroupStatus,
    MessageId::ExtensionsGroupUser,
    MessageId::ExtensionsGroupWorkspace,
    MessageId::ExtensionsHookDetail,
    MessageId::ExtensionsHookFallback,
    MessageId::ExtensionsHooksConfiguration,
    MessageId::ExtensionsInventoryAgents,
    MessageId::ExtensionsInventoryCommands,
    MessageId::ExtensionsInventoryHooks,
    MessageId::ExtensionsInventoryMcp,
    MessageId::ExtensionsInventoryNone,
    MessageId::ExtensionsInventorySkills,
    MessageId::ExtensionPromptUnavailable,
    MessageId::ProfileConstitutionUnavailableLocal,
    MessageId::ExtensionsMarketplaceDetail,
    MessageId::ExtensionsMarketplaceUnavailable,
    MessageId::ExtensionsMcpEmpty,
    MessageId::ExtensionsMcpBrowse,
    MessageId::ExtensionsMcpDetail,
    MessageId::ExtensionsMcpNotInspected,
    MessageId::ExtensionsMcpNotConnectedYet,
    MessageId::ExtensionsMcpRefresh,
    MessageId::ExtensionsMcpSummary,
    MessageId::ExtensionsNoItems,
    MessageId::ExtensionsNoMatches,
    MessageId::ExtensionsPluginDetail,
    MessageId::ExtensionsProductChromeDescription,
    MessageId::ExtensionsProductDetail,
    MessageId::ExtensionsProductPlaywrightDescription,
    MessageId::ExtensionsProductCodewhaleComputerUseDescription,
    MessageId::ExtensionsStateFirstParty,
    MessageId::ExtensionsSearchLabel,
    MessageId::ExtensionsSkillRootCompatibleGlobal,
    MessageId::ExtensionsSkillRootCompatibleProject,
    MessageId::ExtensionsSkillRootConfigured,
    MessageId::ExtensionsSkillRootGlobal,
    MessageId::ExtensionsSkillRootProject,
    MessageId::ExtensionsSkillRootRegistryCache,
    MessageId::ExtensionsSkillRootReviewedPlugin,
    MessageId::ExtensionsStateAvailable,
    MessageId::ExtensionsStateConnected,
    MessageId::ExtensionsStateEnabled,
    MessageId::ExtensionsStateError,
    MessageId::ExtensionsStateInactive,
    MessageId::ExtensionsStateInapplicable,
    MessageId::ExtensionsStateInvalid,
    MessageId::ExtensionsStateNotInspected,
    MessageId::ExtensionsStateDisconnected,
    MessageId::ExtensionsStateRejected,
    MessageId::ExtensionsStateReviewedCandidate,
    MessageId::ExtensionsStateUnderEvaluation,
    MessageId::ExtensionsStateUnstaged,
    MessageId::ExtensionsStateUnsupported,
    MessageId::ExtensionsStateWarning,
    MessageId::ExtensionsTabHooks,
    MessageId::ExtensionsTabMarketplace,
    MessageId::ExtensionsTabMarketplaceCompact,
    MessageId::ExtensionsTabPlugins,
    MessageId::ExtensionsTierCommunity,
    MessageId::ExtensionsTierCurated,
    MessageId::ExtensionsTierOfficial,
    MessageId::ExtensionsTierPartner,
    MessageId::ExtensionsTitle,
    MessageId::ExtensionsTrustCapabilitiesChanged,
    MessageId::ExtensionsTrustContentChanged,
    MessageId::ExtensionsTrustNotReviewed,
    MessageId::ExtensionsTrustTrusted,
    MessageId::ExtensionsValueNo,
    MessageId::ExtensionsValueYes,
    MessageId::PluginKimiUsage,
    MessageId::PluginKimiManagedRootHeading,
    MessageId::PluginKimiNoneFound,
    MessageId::PluginKimiLicenseUnspecified,
    MessageId::PluginKimiApplicable,
    MessageId::PluginKimiNotApplicable,
    MessageId::PluginKimiCandidateSummary,
    MessageId::PluginKimiCandidateDetails,
    MessageId::PluginKimiRejectedHeading,
    MessageId::PluginKimiInspectionFooter,
    MessageId::PluginKimiCandidateMissing,
    MessageId::PluginKimiCandidateChanged,
    MessageId::PluginKimiHomeMissing,
    MessageId::PluginKimiRootInspectFailed,
    MessageId::PluginKimiRootMustBeDirectory,
    MessageId::PluginKimiRootCanonicalizeFailed,
    MessageId::PluginKimiRootListFailed,
    MessageId::PluginKimiEntryReadFailed,
    MessageId::PluginKimiEntryLimit,
    MessageId::PluginKimiEntryInspectFailed,
    MessageId::PluginKimiEntryLinksRefused,
    MessageId::PluginKimiEntryOutsideRoot,
    MessageId::PluginKimiEntryCanonicalizeFailed,
    MessageId::PluginKimiManifestUnreadable,
    MessageId::PluginKimiManifestMustBeFile,
    MessageId::PluginKimiManifestInvalid,
    MessageId::PluginKimiDirectoryNameMismatch,
    MessageId::PluginKimiHashUnavailable,
    MessageId::PluginKimiRollbackDestinationMissing,
    MessageId::PluginKimiMismatchRemoved,
    MessageId::PluginKimiMismatchRollbackFailed,
    MessageId::PluginKimiUserPluginDirectory,
    MessageId::PluginKimiMarketplaceZipUnsupported,
    MessageId::PluginKimiMarketplaceRemoteUnsupported,
    MessageId::PluginKimiMarketplaceGzipTarball,
    MessageId::CmdPluginBundleUsage,
    MessageId::CmdPluginBundleNoneFound,
    MessageId::CmdPluginBundleListHeader,
    MessageId::CmdPluginLegacyListHeader,
    MessageId::CmdPluginBundleNotFound,
    MessageId::CmdPluginBundleReloaded,
    MessageId::PluginPromptSuggestTrust,
    MessageId::PluginPromptSuggestEnable,
    MessageId::PluginPromptSuggestMarketplace,
    MessageId::PluginCtaInstallPrompt,
    MessageId::PluginCtaReview,
    MessageId::PluginCtaInstall,
    MessageId::PluginCtaReviewTrust,
    MessageId::PluginCtaEnable,
    MessageId::PluginCtaDismiss,
    MessageId::PluginCtaDismissSaveFailed,
    MessageId::PluginSuggestionReason,
    MessageId::PagerActionConfirm,
    MessageId::CmdPluginBundleDetail,
    MessageId::CmdPluginOwnerReport,
    MessageId::CmdPluginOwnerActivating,
    MessageId::CmdPluginOwnerActive,
    MessageId::CmdPluginOwnerFailed,
    MessageId::CmdPluginOwnerFaulted,
    MessageId::CmdPluginOwnerRevoked,
    MessageId::CmdPluginOwnerInactive,
    MessageId::CmdPluginBundleDiagnosticsHeader,
    MessageId::CmdPluginBundleMutationSuccess,
    MessageId::CmdPluginActionFailed,
    MessageId::CmdPluginNoneFound,
    MessageId::CmdPluginNotFound,
    MessageId::CmdPluginListHeader,
    MessageId::CmdPluginDetailDescription,
    MessageId::CmdPluginDetailSchema,
    MessageId::CmdPluginDetailApproval,
    MessageId::CmdPluginDetailPath,
    MessageId::CmdMemoryDescription,
    MessageId::CmdModeDescription,
    MessageId::CmdModelDescription,
    MessageId::CmdModelsDescription,
    MessageId::CmdRouterDescription,
    MessageId::RouterSetupTitle,
    MessageId::RouterPresetJevHint,
    MessageId::RouterPresetFastHint,
    MessageId::RouterPresetOffHint,
    MessageId::RouterPresetOffCostSavingHint,
    MessageId::RouterPresetCustomHint,
    MessageId::RouterPresetUnavailable,
    MessageId::RouterTypesafeSignupPaused,
    MessageId::RouterTestSaveHint,
    MessageId::RouterTestHeader,
    MessageId::RouterTestNotMade,
    MessageId::RouterTestWouldRoute,
    MessageId::RouterTestDecision,
    MessageId::RouterTestChoice,
    MessageId::RouterTestCostReported,
    MessageId::RouterTestCostUnreported,
    MessageId::RouterTestUsage,
    MessageId::RouterTestFailing,
    MessageId::RouterSaved,
    MessageId::RouterNotSaved,
    MessageId::RouterRuntimeNotReloaded,
    MessageId::RouterCurrent,
    MessageId::RouterCurrentFailing,
    MessageId::RouterSummary,
    MessageId::RouterSummaryThinking,
    MessageId::RouterPresetFastLabel,
    MessageId::RouterPresetCustomLabel,
    MessageId::RouterNoKey,
    MessageId::RouterNoFastTier,
    MessageId::RouterNeedKey,
    MessageId::RouterCustomByHand,
    MessageId::RouterCustomHelpIntro,
    MessageId::RouterCustomHelpOutro,
    MessageId::RouterActionTest,
    MessageId::RouterActionSave,
    MessageId::RouterActionDiscard,
    MessageId::ModelsListHeader,
    MessageId::ModelsListHint,
    MessageId::ModelsUpdateSummary,
    MessageId::ModelsUpdatePartial,
    MessageId::ModelsCodexHint,
    MessageId::ModelsSourceFallback,
    MessageId::CmdModelDbDescription,
    MessageId::CmdNetworkDescription,
    MessageId::CmdUpdateDescription,
    MessageId::CmdNoteDescription,
    MessageId::CmdProviderDescription,
    MessageId::CmdQueueDescription,
    MessageId::CmdQueueUsage,
    MessageId::CmdQueueDraftHeader,
    MessageId::CmdQueueNoMessages,
    MessageId::CmdQueueListHeader,
    MessageId::CmdQueueTip,
    MessageId::CmdQueueAlreadyEditing,
    MessageId::CmdQueueNotFound,
    MessageId::CmdQueueEditingStatus,
    MessageId::CmdQueueEditingMessage,
    MessageId::CmdQueueDropped,
    MessageId::CmdQueueAlreadyEmpty,
    MessageId::CmdQueueCleared,
    MessageId::CmdQueueMissingIndex,
    MessageId::CmdQueueIndexPositive,
    MessageId::CmdQueueIndexMin,
    MessageId::CmdRelayDescription,
    MessageId::CmdRemoteControlDescription,
    MessageId::CmdRemoteEnvDescription,
    MessageId::CmdRemoteEnvOverview,
    MessageId::CmdRemoteEnvOpening,
    MessageId::CmdRemoteEnvUnavailable,
    MessageId::CmdRemoteEnvSourceCustodyPolicy,
    MessageId::CmdRemoteEnvBrowserLabel,
    MessageId::CmdRenameDescription,
    MessageId::CmdTitleDescription,
    MessageId::CmdRestoreDescription,
    MessageId::CmdRetryDescription,
    MessageId::CmdReviewDescription,
    MessageId::CmdRlmDescription,
    MessageId::CmdSaveDescription,
    MessageId::CmdFullscreenDescription,
    MessageId::CmdInlineDescription,
    MessageId::CmdNewDescription,
    MessageId::CmdSessionsDescription,
    MessageId::CmdSettingsDescription,
    MessageId::CmdSidebarDescription,
    MessageId::CmdSkillDescription,
    MessageId::CmdSkillsDescription,
    MessageId::CmdStashDescription,
    MessageId::CmdStatusDescription,
    MessageId::CmdStatuslineDescription,
    MessageId::CmdStructcopyDescription,
    MessageId::CmdStructcopyKindTurn,
    MessageId::CmdStructcopyKindTool,
    MessageId::CmdStructcopyKindPlan,
    MessageId::CmdStructcopyKindWorkflow,
    MessageId::CmdStructcopyUsageError,
    MessageId::CmdStructcopyUnavailable,
    MessageId::CmdStructcopyBusy,
    MessageId::CmdStructcopyPrepareFailed,
    MessageId::CmdStructcopyClipboardQueued,
    MessageId::CmdStructcopyClipboardAccepted,
    MessageId::CmdStructcopyClipboardFailed,
    MessageId::CmdStructcopyReceiptTooLarge,
    MessageId::CmdFleetDescription,
    MessageId::PetUnobserved,
    MessageId::PetOffline,
    MessageId::PetDozing,
    MessageId::PetWatchUnavailable,
    MessageId::PetWatchRestored,
    MessageId::PetWatchStorageUnavailable,
    MessageId::PetWatchExported,
    MessageId::PetWatchExportFailed,
    MessageId::PetWatchExportQueued,
    MessageId::PetWatchExportUnavailable,
    MessageId::PetHabitatTitle,
    MessageId::PetHabitatHints,
    MessageId::PetHabitatQueued,
    MessageId::PetWatchSoundOn,
    MessageId::PetWatchSoundOff,
    MessageId::PetWatchSoundPaused,
    MessageId::PetWatchSoundUnavailable,
    MessageId::CmdPetDescription,
    MessageId::ConfigLabelPetMode,
    MessageId::ConfigHintPetMode,
    MessageId::PetModeCopyHint,
    MessageId::PetModeOn,
    MessageId::PetModeOff,
    MessageId::PetModeOnLabel,
    MessageId::PetModeInspectHint,
    MessageId::PetModeReplyTitle,
    MessageId::PetModeMessageHint,
    MessageId::PetModePaneHint,
    MessageId::PetModeAgentHints,
    MessageId::PetModeReplyHints,
    MessageId::PetModeCompanionHints,
    MessageId::PetModeOffLabel,
    MessageId::PetViewOpen,
    MessageId::PetViewClosed,
    MessageId::CmdLaneDescription,
    MessageId::CmdWorkflowDescription,
    MessageId::CmdWorkflowsDescription,
    MessageId::CmdAutoDescription,
    MessageId::AutoReceiptOn,
    MessageId::AutoReceiptPlanNote,
    MessageId::CmdHotbarDescription,
    MessageId::CmdSetupDescription,
    MessageId::CmdSubagentsDescription,
    MessageId::CmdAdvisorDescription,
    MessageId::CmdSystemDescription,
    MessageId::CmdAutomationDescription,
    MessageId::CmdTaskDescription,
    MessageId::CmdTokensDescription,
    MessageId::CmdTranslateDescription,
    MessageId::CmdTranslateOff,
    MessageId::CmdTranslateOn,
    MessageId::TranslationInProgress,
    MessageId::TranslationComplete,
    MessageId::TranslationFailed,
    MessageId::CmdTrustDescription,
    MessageId::CmdLspDescription,
    MessageId::CmdShareDescription,
    MessageId::CmdWorkspaceDescription,
    MessageId::CmdUndoDescription,
    MessageId::CmdVerboseDescription,
    MessageId::CmdCacheAdvice,
    MessageId::CmdCacheFootnote,
    MessageId::CmdCacheHeader,
    MessageId::CmdCacheNoData,
    MessageId::CmdCacheSessionRates,
    MessageId::CmdCacheRateParent,
    MessageId::CmdCacheRateAgents,
    MessageId::CmdCacheRateCombined,
    MessageId::CmdCacheTotals,
    MessageId::CmdChangeDescription,
    MessageId::CmdChangeHeader,
    MessageId::CmdChangeTranslationQueued,
    MessageId::CmdChangeTranslationUnavailable,
    MessageId::CmdChangePreviousVersion,
    MessageId::CmdCostReport,
    MessageId::CmdCostReportSubtotal,
    MessageId::CmdCostReportUnknown,
    MessageId::CmdCostUnknownValue,
    MessageId::CmdCostEstimateOnly,
    MessageId::CmdCostCoverage,
    MessageId::CmdCostCoverageUnknownLegacy,
    MessageId::CmdCostUnpricedTurns,
    MessageId::CmdCostUnpricedClasses,
    MessageId::CmdCostPricingProvenance,
    MessageId::CmdCostLivePricingDowngraded,
    MessageId::CmdCostLivePricingUnavailable,
    MessageId::CmdCostRoutesHeader,
    MessageId::CmdTokensCacheWriteTotal,
    MessageId::CmdTokensCacheBoth,
    MessageId::CmdTokensCacheHitOnly,
    MessageId::CmdTokensCacheMissOnly,
    MessageId::CmdTokensContextUnknownWindow,
    MessageId::CmdTokensContextWithWindow,
    MessageId::CmdTokensNotReported,
    MessageId::CmdTokensReport,
    MessageId::FooterAgentSingular,
    MessageId::FooterAgentsPlural,
    MessageId::HeaderAgentsChip,
    MessageId::FooterPressCtrlCAgain,
    MessageId::FooterWorking,
    MessageId::FooterBalancePrefix,
    MessageId::HelpSectionActions,
    MessageId::HelpSectionClipboard,
    MessageId::HelpSectionPointer,
    MessageId::HelpSectionEditing,
    MessageId::HelpSectionHelp,
    MessageId::HelpSectionModes,
    MessageId::HelpSectionNavigation,
    MessageId::HelpSectionSessions,
    MessageId::KbScrollTranscript,
    MessageId::KbNavigateHistory,
    MessageId::KbScrollTranscriptAlt,
    MessageId::KbBrowseHistory,
    MessageId::KbScrollPage,
    MessageId::KbJumpTopBottom,
    MessageId::KbJumpTopBottomEmpty,
    MessageId::KbJumpToolBlocks,
    MessageId::KbMoveCursor,
    MessageId::KbJumpLineStartEnd,
    MessageId::KbDeleteChar,
    MessageId::KbDeleteWord,
    MessageId::KbYank,
    MessageId::KbToggleFileTree,
    MessageId::KbSelectText,
    MessageId::KbSelectAllDraft,
    MessageId::KbClearDraft,
    MessageId::KbRestoreClearedDraft,
    MessageId::KbStashDraft,
    MessageId::KbSearchHistory,
    MessageId::KbInsertNewline,
    MessageId::KbSendDraft,
    MessageId::KbSteerCurrentTurn,
    MessageId::KbCloseMenu,
    MessageId::KbCancelOrExit,
    MessageId::KbShellControls,
    MessageId::KbExitEmpty,
    MessageId::KbCommandPalette,
    MessageId::KbSettings,
    MessageId::KbCancelBackgroundShellJobs,
    MessageId::KbFuzzyFilePicker,
    MessageId::KbCompactInspector,
    MessageId::KbCompactContext,
    MessageId::KbLastMessagePager,
    MessageId::KbSelectedDetails,
    MessageId::KbToolDetailsPager,
    MessageId::KbReasoningDetail,
    MessageId::KbTurnInspector,
    MessageId::KbExternalEditor,
    MessageId::KbLiveTranscript,
    MessageId::KbBacktrackMessage,
    MessageId::KbCompleteCycleModes,
    MessageId::KbCycleThinking,
    MessageId::KbCyclePermissions,
    MessageId::KbJumpPlanAgentYolo,
    MessageId::KbAltJumpPlanAgentYolo,
    MessageId::KbFocusSidebar,
    MessageId::KbSessionPicker,
    MessageId::KbUpdateInstall,
    MessageId::UpdateChangedHint,
    MessageId::KbTerminalPaste,
    MessageId::KbPasteAttach,
    MessageId::KbCopySelection,
    MessageId::ClipboardSshPasteHint,
    MessageId::ClipboardCopied,
    MessageId::ClipboardSentToTerminal,
    MessageId::ClipboardCutKeptText,
    MessageId::ClipboardNothingToPaste,
    MessageId::ComposerOversizedSubmitHeld,
    MessageId::ComposerOversizedSubmitHeldShort,
    MessageId::KbContextMenu,
    MessageId::KbPointerScroll,
    MessageId::KbPointerClick,
    MessageId::KbPointerDrag,
    MessageId::KbAttachPath,
    MessageId::KbHelpOverlay,
    MessageId::KbCycleWorkDock,
    MessageId::KbCycleWorkDockBack,
    MessageId::KbToggleHelp,
    MessageId::KbToggleHelpSlash,
    MessageId::HelpUsageLabel,
    MessageId::HelpAliasesLabel,
    MessageId::SettingsTitle,
    MessageId::SettingsConfigFile,
    MessageId::SettingsTuiPrefsFolded,
    MessageId::SettingsTuiPrefsKept,
    MessageId::SettingsTuiPrefsQuarantined,
    MessageId::ClearConversation,
    MessageId::ClearConversationBusy,
    MessageId::ModelChanged,
    MessageId::ModelChangedSessionNote,
    MessageId::LinksProjectTitle,
    MessageId::LinksDocumentation,
    MessageId::LinksCommunity,
    MessageId::LinksGitHub,
    MessageId::LinksManagedApp,
    MessageId::LinksManagedAppNote,
    MessageId::LinksTitle,
    MessageId::LinksDashboard,
    MessageId::LinksDocs,
    MessageId::LinksKimiCodeRouteNote,
    MessageId::LinksTip,
    MessageId::WorkspaceCurrent,
    MessageId::WorkspaceNotFound,
    MessageId::WorkspaceNotDirectory,
    MessageId::WorkspaceSwitching,
    MessageId::WorkspaceHomeUnresolved,
    MessageId::WorkspaceSwitchBusy,
    MessageId::WorkspaceUnchanged,
    MessageId::WorkspaceSwitched,
    MessageId::WorkspaceStatus,
    MessageId::SubagentsFetching,
    MessageId::SubagentsNoCurrentSessionFleetWorkers,
    MessageId::SubagentsCurrentSessionFleetWorkersTitle,
    MessageId::SubagentsCurrentSessionFleetWorkerRoles,
    MessageId::SubagentsCurrentSessionFleetWorkersStatus,
    MessageId::SubagentsEmptyGuidance,
    MessageId::SubagentsStatusRunning,
    MessageId::SubagentsStatusCompleted,
    MessageId::SubagentsStatusInterrupted,
    MessageId::SubagentsStatusFailed,
    MessageId::SubagentsStatusCancelled,
    MessageId::SubagentsRowStatusInterrupted,
    MessageId::SubagentsRowStatusCancelled,
    MessageId::SubagentsRowStatusBudgetExhausted,
    MessageId::SubagentsSummaryItem,
    MessageId::SubagentsGroupHeading,
    MessageId::SubagentsHeaderRoster,
    MessageId::SubagentsHeaderColumns,
    MessageId::SubagentsActionRefresh,
    MessageId::SubagentsActionRosterSetup,
    MessageId::SubagentsLabelReason,
    MessageId::SubagentsLabelRole,
    MessageId::SubagentsLabelPosture,
    MessageId::SubagentsLabelGit,
    MessageId::SubagentsLabelObjective,
    MessageId::SubagentsLabelResult,
    MessageId::SubagentsPostureDetails,
    MessageId::SubagentsValueOn,
    MessageId::SubagentsValueOff,
    MessageId::SubagentsShellNone,
    MessageId::SubagentsShellReadOnly,
    MessageId::SubagentsShellFull,
    MessageId::SubagentsBranch,
    MessageId::SubagentsBranchWithWorkspace,
    MessageId::SubagentsRoleWorker,
    MessageId::SubagentsRoleScout,
    MessageId::SubagentsRolePlanner,
    MessageId::SubagentsRoleBuilder,
    MessageId::SubagentsRoleVerifier,
    MessageId::SubagentsRoleReviewer,
    MessageId::SubagentsRoleConsultant,
    MessageId::SubagentsRoleCustom,
    MessageId::HelpUnknownCommand,
    MessageId::HomeDashboardTitle,
    MessageId::HomeModel,
    MessageId::HomeMode,
    MessageId::HomeWorkspace,
    MessageId::HomeHistory,
    MessageId::HomeTokens,
    MessageId::HomeQueued,
    MessageId::HomeSubagents,
    MessageId::HomeSkill,
    MessageId::HomeQuickActions,
    MessageId::HomeQuickLinks,
    MessageId::HomeQuickSkills,
    MessageId::HomeQuickConfig,
    MessageId::HomeQuickSettings,
    MessageId::HomeQuickModel,
    MessageId::HomeQuickSubagents,
    MessageId::HomeQuickTaskList,
    MessageId::HomeQuickHelp,
    MessageId::HomeQuickWorkspace,
    MessageId::HomeQuickRestore,
    MessageId::HomeQuickTokens,
    MessageId::HomeModeTips,
    MessageId::HomeAgentModeTip,
    MessageId::HomeAgentModeReviewTip,
    MessageId::HomeAgentModeYoloTip,
    MessageId::HomeYoloModeTip,
    MessageId::HomeYoloModeCaution,
    MessageId::HomePlanModeTip,
    MessageId::HomePlanModeChecklistTip,
    MessageId::HomeOperateModeTip,
    MessageId::HomeOperateModeFleetTip,
    MessageId::HomeGoalModeTip,
    MessageId::OnboardWelcomeTitle,
    MessageId::OnboardWelcomeLead,
    MessageId::OnboardWelcomeBegin,
    MessageId::OnboardActionBack,
    MessageId::OnboardActionExit,
    MessageId::OnboardStepsTitle,
    MessageId::OnboardLanguageTitle,
    MessageId::OnboardLanguageBlurb,
    MessageId::OnboardLanguagePick,
    MessageId::OnboardLanguageKeep,
    MessageId::OnboardProviderTitle,
    MessageId::OnboardProviderBlurb,
    MessageId::OnboardProviderChoose,
    MessageId::OnboardProviderOffline,
    MessageId::KimiCodePlanApiKeyHint,
    MessageId::KimiCodePlanRouteHint,
    MessageId::KimiCodePlanNoImportHint,
    MessageId::StepfunBillingRouteTitle,
    MessageId::StepfunBillingRouteIntro,
    MessageId::StepfunBillingRoutePaygOption,
    MessageId::StepfunBillingRoutePlanOption,
    MessageId::StepfunPlanApiKeyHint,
    MessageId::StepfunPlanRouteHint,
    MessageId::OnboardApiKeyRejectedEnv,
    MessageId::OnboardTrustTitle,
    MessageId::OnboardTrustQuestion,
    MessageId::OnboardTrustLocationPrefix,
    MessageId::OnboardTrustRiskHint,
    MessageId::OnboardTrustEffectHint,
    MessageId::OnboardTrustActionTrust,
    MessageId::OnboardTrustActionSkip,
    MessageId::OnboardTrustActionQuit,
    MessageId::OnboardTrustEnterHint,
    MessageId::OnboardTrustUntrustedNotice,
    MessageId::RedactionGateSaveFailed,
    MessageId::RedactionGateTitle,
    MessageId::RedactionGateQuestion,
    MessageId::RedactionGateDangerNotice,
    MessageId::RedactionGateRisk,
    MessageId::RedactionGateEffect,
    MessageId::RedactionGateRollbackHint,
    MessageId::RedactionGateEnterHint,
    MessageId::RedactionGateActionConfirm,
    MessageId::RedactionGateActionKeep,
    MessageId::RedactionGateActionQuit,
    MessageId::RedactionGateConfirmTitle,
    MessageId::RedactionGateConfirmQuestion,
    MessageId::RedactionGateActionBack,
    MessageId::OnboardOfflineOption,
    MessageId::OnboardOfflineNotice,
    MessageId::OnboardReadyTitle,
    MessageId::OnboardReadyLead,
    MessageId::OnboardReadyStart,
    MessageId::OnboardReadyCustomize,
    MessageId::OnboardSeedCodeProject,
    MessageId::OnboardSeedFolder,
    MessageId::SetupWizardTitle,
    MessageId::SetupWizardWhy,
    MessageId::SetupWizardProgress,
    MessageId::SetupActionBack,
    MessageId::SetupActionContinue,
    MessageId::SetupActionSkip,
    MessageId::SetupActionRetry,
    MessageId::SetupActionScrollBody,
    MessageId::SetupActionGuided,
    MessageId::SetupActionTuneGuided,
    MessageId::SetupActionModelDraft,
    MessageId::SetupActionFreeform,
    MessageId::SetupActionKeepExisting,
    MessageId::SetupActionUseRecommended,
    MessageId::SetupActionCustomize,
    MessageId::SetupActionProvider,
    MessageId::SetupActionModel,
    MessageId::SetupActionFleet,
    MessageId::SetupActionHotbar,
    MessageId::SetupActionRemote,
    MessageId::SetupActionMode,
    MessageId::SetupActionConfig,
    MessageId::SetupActionRuntimePreset,
    MessageId::SetupActionApplyRuntimePreset,
    MessageId::SetupActionUseBundled,
    MessageId::SetupActionDefer,
    MessageId::SetupActionCancel,
    MessageId::SetupStatusNotStarted,
    MessageId::SetupStatusRecommended,
    MessageId::SetupStatusOptional,
    MessageId::SetupStatusDeferred,
    MessageId::SetupStatusInProgress,
    MessageId::SetupStatusNeedsAction,
    MessageId::SetupStatusVerified,
    MessageId::SetupStatusSkipped,
    MessageId::SetupStatusFailed,
    MessageId::SetupStepLanguageTitle,
    MessageId::SetupStepLanguageWhy,
    MessageId::SetupStepProviderModelTitle,
    MessageId::SetupStepProviderModelWhy,
    MessageId::SetupStepTrustSandboxTitle,
    MessageId::SetupStepTrustSandboxWhy,
    MessageId::SetupStepOperateFleetTitle,
    MessageId::SetupStepOperateFleetWhy,
    MessageId::SetupStepToolsMcpTitle,
    MessageId::SetupStepToolsMcpWhy,
    MessageId::SetupStepHotbarTitle,
    MessageId::SetupStepHotbarWhy,
    MessageId::SetupStepRemoteRuntimeTitle,
    MessageId::SetupStepRemoteRuntimeWhy,
    MessageId::SetupStepPersistenceTitle,
    MessageId::SetupStepPersistenceWhy,
    MessageId::SetupStepConstitutionTitle,
    MessageId::SetupStepConstitutionWhy,
    MessageId::SetupStepVerificationTitle,
    MessageId::SetupStepVerificationWhy,
    MessageId::SetupCheckpointLayerOrder,
    MessageId::SetupCheckpointDoneBundled,
    MessageId::SetupCheckpointDoneGuided,
    MessageId::SetupCheckpointDoneKept,
    MessageId::SetupCheckpointDeferred,
    MessageId::SetupStepSkipped,
    MessageId::SetupStepRetryRecorded,
    MessageId::SetupLanguageReviewed,
    MessageId::SetupConstitutionChoiceLabel,
    MessageId::SetupConstitutionSourceLabel,
    MessageId::SetupConstitutionValidityLabel,
    MessageId::SetupConstitutionPreviewLabel,
    MessageId::SetupConstitutionExistingLabel,
    MessageId::SetupConstitutionExpertOverrideLabel,
    MessageId::SetupConstitutionGuidedHint,
    MessageId::SetupConstitutionGuidedAnswersHint,
    MessageId::SetupConstitutionExistingDefaultDetail,
    MessageId::SetupConstitutionRepairDefaultDetail,
    MessageId::SetupConstitutionPurposeLabel,
    MessageId::SetupConstitutionAutonomyLabel,
    MessageId::SetupConstitutionEvidenceLabel,
    MessageId::SetupConstitutionCommunicationLabel,
    MessageId::SetupConstitutionPrivacyLabel,
    MessageId::SetupConstitutionPrinciplesLabel,
    MessageId::SetupCardRouteLabel,
    MessageId::SetupCardModelLabel,
    MessageId::SetupCardAuthLabel,
    MessageId::SetupCardHealthLabel,
    MessageId::SetupCardIntentLabel,
    MessageId::SetupCardApprovalLabel,
    MessageId::SetupCardShellLabel,
    MessageId::SetupCardTrustLabel,
    MessageId::SetupCardSandboxLabel,
    MessageId::SetupCardNetworkLabel,
    MessageId::SetupOperateRuntimeLabel,
    MessageId::SetupOperateRosterLabel,
    MessageId::SetupOperateConcurrencyLabel,
    MessageId::SetupOperateReadinessLabel,
    MessageId::SetupOperateReviewHint,
    MessageId::SetupOperateReviewed,
    MessageId::SetupOperateNeedsActionSaved,
    MessageId::SetupHotbarBindingsLabel,
    MessageId::SetupHotbarActionsLabel,
    MessageId::SetupHotbarReviewHint,
    MessageId::SetupHotbarReviewed,
    MessageId::SetupToolsMcpServersLabel,
    MessageId::SetupToolsMcpSkillsLabel,
    MessageId::SetupToolsMcpToolsLabel,
    MessageId::SetupToolsMcpPluginsLabel,
    MessageId::SetupToolsMcpHotbarLabel,
    MessageId::SetupToolsMcpReviewHint,
    MessageId::SetupToolsMcpReviewed,
    MessageId::SetupToolsMcpNeedsActionSaved,
    MessageId::SetupToolsMcpPreviewTitle,
    MessageId::SetupToolsMcpOnRampText,
    MessageId::SetupToolsMcpDshLabel,
    MessageId::SetupToolsMcpDshRow,
    MessageId::SetupRemoteCloudsLabel,
    MessageId::SetupRemoteBridgesLabel,
    MessageId::SetupRemoteProvidersLabel,
    MessageId::SetupRemoteModeLabel,
    MessageId::SetupRemoteModeLocalOnly,
    MessageId::SetupRemoteModeRuntimeApi,
    MessageId::SetupRemoteModeMobileLan,
    MessageId::SetupRemoteModeChatBridge,
    MessageId::SetupRemoteStatusDisabled,
    MessageId::SetupRemoteStatusReady,
    MessageId::SetupRemoteStatusNeedsAction,
    MessageId::SetupRemoteReviewHint,
    MessageId::SetupRemotePreviewTitle,
    MessageId::SetupRemoteReviewed,
    MessageId::SetupPersistenceHomeLabel,
    MessageId::SetupPersistenceConfigLabel,
    MessageId::SetupPersistenceStateLabel,
    MessageId::SetupPersistenceConstitutionLabel,
    MessageId::SetupPersistenceMemoryLabel,
    MessageId::SetupPersistenceNotesLabel,
    MessageId::SetupPersistenceReviewHint,
    MessageId::SetupPersistenceReviewed,
    MessageId::SetupProviderModelReadyHint,
    MessageId::SetupProviderModelNeedsActionHint,
    MessageId::SetupProviderModelReviewed,
    MessageId::SetupProviderModelNeedsActionSaved,
    MessageId::SetupRuntimePostureBoundary,
    MessageId::SetupProjectPermissionsInvalid,
    MessageId::SetupProjectPermissionsOverride,
    MessageId::SetupRuntimePostureReviewHint,
    MessageId::SetupRuntimePostureReviewed,
    MessageId::SetupRuntimePresetSelectedLabel,
    MessageId::SetupRuntimePresetDiffLabel,
    MessageId::SetupRuntimePresetAskFirstTitle,
    MessageId::SetupRuntimePresetAskFirstDescription,
    MessageId::SetupRuntimePresetNormalAgentTitle,
    MessageId::SetupRuntimePresetNormalAgentDescription,
    MessageId::SetupRuntimePresetHighTrustTitle,
    MessageId::SetupRuntimePresetHighTrustDescription,
    MessageId::SetupRuntimePresetPreviewTitle,
    MessageId::SetupRuntimePresetSafetyFloor,
    MessageId::SetupRuntimePresetApplyHint,
    MessageId::SetupRuntimePresetApplied,
    MessageId::SetupRuntimeProjectOverrideLabel,
    MessageId::SetupRuntimeProjectOverrideNone,
    MessageId::SetupReportFirstRunLabel,
    MessageId::SetupReportUpdateLabel,
    MessageId::SetupReportOperateLabel,
    MessageId::SetupReportSourceLabel,
    MessageId::SetupReportAutonomyLabel,
    MessageId::SetupReportRuntimePostureLabel,
    MessageId::SetupReportPersisted,
    MessageId::SetupReportInherited,
    MessageId::SetupReportReady,
    MessageId::SetupReportRequired,
    MessageId::SetupReportOptional,
    MessageId::SetupReportRowsLabel,
    MessageId::SetupReportNextActionLabel,
    MessageId::SetupReportNextActionNone,
    MessageId::SetupReportNextActionConstitution,
    MessageId::SetupReportNextActionProvider,
    MessageId::SetupReportNextActionRuntime,
    MessageId::SetupReportNextActionOperate,
    MessageId::SetupReportNextActionRequired,
    MessageId::SetupReportRecorded,
    // Context menu.
    MessageId::CtxMenuTitle,
    MessageId::CtxMenuCopySelection,
    MessageId::CtxMenuCopySelectionDesc,
    MessageId::CtxMenuOpenSelection,
    MessageId::CtxMenuOpenSelectionDesc,
    MessageId::CtxMenuClearSelection,
    MessageId::CtxMenuOpenDetails,
    MessageId::CtxMenuCopyMessage,
    MessageId::CtxMenuCopyMessageDesc,
    MessageId::CtxMenuOpenInEditor,
    MessageId::CtxMenuOpenInEditorDesc,
    MessageId::CtxMenuShowCell,
    MessageId::CtxMenuShowCellDesc,
    MessageId::CtxMenuHideCell,
    MessageId::CtxMenuHideCellDesc,
    MessageId::CtxMenuShowHidden,
    MessageId::CtxMenuShowHiddenDesc,
    MessageId::CtxMenuPaste,
    MessageId::CtxMenuPasteDesc,
    MessageId::CtxMenuCmdPalette,
    MessageId::CtxMenuCmdPaletteDesc,
    MessageId::CtxMenuContextInspector,
    MessageId::CtxMenuContextInspectorDesc,
    MessageId::CtxMenuHelp,
    MessageId::CtxMenuHelpDesc,
    MessageId::CtxMenuWindowPin,
    MessageId::CtxMenuWindowUnpin,
    MessageId::CtxMenuWindowPinDesc,
    MessageId::CtxMenuRunCommand,
    MessageId::CtxMenuOpen,
    MessageId::CtxMenuFocusAgent,
    MessageId::CtxMenuFocusAgentDesc,
    MessageId::CtxMenuCopyId,
    MessageId::CtxMenuCopyRow,
    MessageId::CtxMenuStopAgent,
    MessageId::CtxMenuStopWork,
    MessageId::CtxMenuConfirmArmed,
    MessageId::CtxMenuEditorRefused,
    MessageId::CmdPinDescription,
    MessageId::WindowPinActive,
    MessageId::WindowPinReleased,
    MessageId::WindowPinFailed,
    MessageId::FanoutCounts,
    MessageId::AppModeAgent,
    MessageId::AppModeAuto,
    MessageId::AppModeYolo,
    MessageId::AppModePlan,
    MessageId::AppModeOperate,
    MessageId::AppModeAgentHint,
    MessageId::AppModeAutoHint,
    MessageId::AppModePlanHint,
    MessageId::AppModeYoloHint,
    MessageId::AppModeOperateHint,
    MessageId::VimModeNormal,
    MessageId::VimModeInsert,
    MessageId::VimModeVisual,
    MessageId::ApprovalEffectReadsOnly,
    MessageId::ApprovalEffectChangesFiles,
    MessageId::ApprovalRiskDestructive,
    MessageId::ApprovalEffectRunsCommand,
    MessageId::ApprovalEffectUsesNetwork,
    MessageId::ApprovalEffectConnectedApp,
    MessageId::ApprovalEffectStartsAgent,
    MessageId::ApprovalEffectUnclassified,
    MessageId::ApprovalCategorySafe,
    MessageId::ApprovalCategoryFileWrite,
    MessageId::ApprovalCategoryShell,
    MessageId::ApprovalCategoryNetwork,
    MessageId::ApprovalCategoryMcpRead,
    MessageId::ApprovalCategoryMcpAction,
    MessageId::ApprovalCategoryAgent,
    MessageId::ApprovalCategoryUnknown,
    MessageId::ApprovalFieldType,
    MessageId::ApprovalFieldAbout,
    MessageId::ApprovalFieldImpact,
    MessageId::ApprovalFieldParams,
    MessageId::ApprovalOptionApproveOnce,
    MessageId::ApprovalOptionApproveAlways,
    MessageId::ApprovalOptionAllowExactRepo,
    MessageId::ApprovalSaveAskRuleHint,
    MessageId::ApprovalOptionDeny,
    MessageId::ApprovalOptionAbortTurn,
    MessageId::ApprovalBlockTitle,
    MessageId::ApprovalControlsHint,
    MessageId::ApprovalOwnerHeader,
    MessageId::ApprovalGoToAgent,
    MessageId::ApprovalControlsHintChild,
    MessageId::PendingApprovalInAgent,
    MessageId::ApprovalTruncationHint,
    MessageId::ApprovalAsksWithoutGit,
    MessageId::ApprovalFullAccessPolicyBlocked,
    MessageId::ApprovalChooseHint,
    MessageId::ApprovalChooseAction,
    MessageId::ApprovalIntentLabel,
    MessageId::ApprovalMoreLines,
    MessageId::ApprovalAutoDeniedSession,
    MessageId::ElevationTitleSandboxDenied,
    MessageId::ElevationTitleRequired,
    MessageId::ElevationFieldTool,
    MessageId::ElevationFieldCmd,
    MessageId::ElevationFieldReason,
    MessageId::ElevationImpactHeader,
    MessageId::ElevationImpactNetwork,
    MessageId::ElevationImpactWrite,
    MessageId::ElevationImpactFullAccess,
    MessageId::ElevationPromptProceed,
    MessageId::ElevationOptionNetwork,
    MessageId::ElevationOptionWrite,
    MessageId::ElevationOptionFullAccess,
    MessageId::ElevationOptionAbort,
    MessageId::ElevationOptionNetworkDesc,
    MessageId::ElevationOptionWriteDesc,
    MessageId::ElevationOptionFullAccessDesc,
    MessageId::ElevationOptionAbortDesc,
    MessageId::ContextAutoCompacting,
    MessageId::ContextManualCompacting,
    MessageId::ContextCompactionQueued,
    MessageId::ContextCompactionAlreadyRunning,
    MessageId::ContextCompactionQueueFull,
    MessageId::ContextCompactionQueueClosed,
    MessageId::ContextCompactionRouteInvalid,
    MessageId::CtxInspTitle,
    MessageId::CtxInspSessionContext,
    MessageId::CtxInspSystemPrompt,
    MessageId::CtxInspReferences,
    MessageId::CtxInspRecentTools,
    MessageId::CtxInspToolSchemaCosts,
    MessageId::CtxInspModel,
    MessageId::CtxInspWorkspace,
    MessageId::CtxInspSession,
    MessageId::CtxInspContext,
    MessageId::CtxInspTranscript,
    MessageId::CtxInspWorkspaceStatus,
    MessageId::CtxInspNotSampledYet,
    MessageId::CtxInspOk,
    MessageId::CtxInspHigh,
    MessageId::CtxInspCritical,
    MessageId::CtxInspIncluded,
    MessageId::CtxInspAttached,
    MessageId::CtxInspNotIncluded,
    MessageId::CtxInspOutputCaptured,
    MessageId::CtxInspNoOutputYet,
    MessageId::CtxInspNoSystemPrompt,
    MessageId::CtxInspNoReferences,
    MessageId::CtxInspNoToolActivity,
    MessageId::CtxInspVHint,
    MessageId::CtxInspCells,
    MessageId::CtxInspApiMessages,
    MessageId::CtxInspActive,
    MessageId::CtxInspCell,
    MessageId::CtxInspMoreReferences,
    MessageId::CtxInspStablePrefix,
    MessageId::CtxInspVolatileWorkingSet,
    MessageId::CtxInspFirstLine,
    MessageId::CtxInspTotal,
    MessageId::CtxInspTextPromptLayers,
    MessageId::CtxInspSingleTextBlob,
    MessageId::CtxInspBlocks,
    MessageId::CtxInspBlock,
    MessageId::CtxInspTokens,
    MessageId::CtxInspLayers,
    MessageId::CtxInspNone,
    MessageId::CtxInspEmpty,
    MessageId::CtxInspCacheFriendly,
    MessageId::CtxInspChangesByTurn,
    MessageId::CtxInspStablePrefixOnly,
    MessageId::CtxInspCacheTip,
    MessageId::ToolFamilyRead,
    MessageId::ToolFamilyPatch,
    MessageId::ToolFamilyRun,
    MessageId::ToolFamilyFind,
    MessageId::ToolFamilyDelegate,
    MessageId::ToolFamilyFanout,
    MessageId::ToolFamilyRlm,
    MessageId::ToolFamilyVerify,
    MessageId::ToolFamilyThink,
    MessageId::ToolFamilyGeneric,
    MessageId::ToolReceiptDone,
    MessageId::ToolReceiptLinesSingular,
    MessageId::ToolReceiptLinesPlural,
    MessageId::CmdVoiceDescription,
    MessageId::CmdVoiceSendDescription,
    MessageId::CmdVoiceControlDescription,
    MessageId::VoiceEnabled,
    MessageId::VoiceDisabled,
    MessageId::VoiceSendEnabled,
    MessageId::VoiceSendDisabled,
    MessageId::VoiceControlEnabled,
    MessageId::VoiceControlDisabled,
    MessageId::VoiceErrNoAuth,
    MessageId::VoiceErrNoRecorder,
    MessageId::VoiceErrNetwork,
    MessageId::VoiceErrEmptySend,
    MessageId::VoiceErrTooShort,
    MessageId::VoiceRecording,
    MessageId::VoiceRecordingStopHint,
    MessageId::VoiceProcessing,
    MessageId::VoiceTranscribed,
    MessageId::NotificationApprovalNeeded,
    MessageId::NotificationInputNeeded,
    MessageId::NotificationElevationNeeded,
    MessageId::NotificationDecisionWebHint,
    MessageId::NotificationTurnFailed,
    MessageId::NotificationProviderFallback,
    MessageId::ApprovalNeverPostureBlocked,
    MessageId::ApprovalTimedOutDenied,
    MessageId::NotificationWebApproved,
    MessageId::NotificationWebDenied,
    MessageId::NotificationInputSubmitFailed,
    MessageId::NotificationTurnComplete,
    MessageId::NotificationSubagentComplete,
    MessageId::NotificationSubagentFailed,
    MessageId::NotificationSubagentInterrupted,
    MessageId::NotificationSubagentCancelled,
    MessageId::NotificationSubagentBudgetExhausted,
    MessageId::NotificationShellFinished,
    MessageId::NotificationShellFailed,
    MessageId::NotificationShellStopped,
    MessageId::ShellWaitNoActive,
    MessageId::ShellWaitInteractive,
    MessageId::ShellWaitUnavailable,
    MessageId::ShellWaitReleased,
    MessageId::NotificationTaskFinished,
    MessageId::NotificationTaskFailed,
    MessageId::NotificationTaskStopped,
    MessageId::NotificationBackgroundFinished,
    MessageId::NotificationBackgroundMixed,
    MessageId::BackgroundFinishedHeading,
    MessageId::BackgroundFinishedHint,
    MessageId::BackgroundOutcomeDone,
    MessageId::BackgroundOutcomeFailed,
    MessageId::BackgroundOutcomeCancelled,
    MessageId::BackgroundOutcomeKilled,
    MessageId::BackgroundOutcomeTimedOut,
    MessageId::BackgroundExitCode,
    MessageId::BackgroundQuiet,
    MessageId::BackgroundUsingTool,
    MessageId::BackgroundStep,
    MessageId::BackgroundFilesChanged,
    MessageId::NotificationBackgroundStopped,
    MessageId::NotificationFullResultPointer,
    MessageId::FooterWorkedChip,
    MessageId::FooterPermissionKeyHint,
    MessageId::ApprovalSummaryRunCommand,
    MessageId::ApprovalSummaryRunShell,
    MessageId::ApprovalSummaryShellWait,
    MessageId::ApprovalSummaryShellInput,
    MessageId::ApprovalSummaryShellStop,
    MessageId::ApprovalSummaryWritePath,
    MessageId::ApprovalSummaryWriteFile,
    MessageId::ApprovalSummaryEditPath,
    MessageId::ApprovalSummaryEditFile,
    MessageId::ApprovalSummaryEditPathMore,
    MessageId::ApprovalSummaryApplyPatch,
    MessageId::ApprovalSummaryReadPath,
    MessageId::ApprovalSummaryReadFile,
    MessageId::ApprovalSummaryListPath,
    MessageId::ApprovalSummaryListWorkspace,
    MessageId::ApprovalSummaryFetchUrl,
    MessageId::ApprovalSummaryFetchPage,
    MessageId::ApprovalSummarySearchQuery,
    MessageId::ApprovalSummarySearchWeb,
    MessageId::ApprovalSummaryImageQuery,
    MessageId::ApprovalSummaryOpenTarget,
    MessageId::ApprovalSummaryMore,
    MessageId::ApprovalSummaryFollowLink,
    MessageId::ApprovalSummaryFindOnPage,
    MessageId::ApprovalSummaryScreenshot,
    MessageId::ApprovalSummaryBrowse,
    MessageId::ApprovalSummaryMcpTool,
    MessageId::ApprovalSummaryUseName,
    MessageId::ApprovalSummaryUseTool,
    MessageId::FleetDraftTitle,
    MessageId::FleetDraftHeader,
    MessageId::SetupRemoteOnRampText,
    MessageId::ApprovalDescSafe,
    MessageId::ApprovalDescFileWrite,
    MessageId::ApprovalDescShell,
    MessageId::ApprovalDescNetwork,
    MessageId::ApprovalDescMcpRead,
    MessageId::ApprovalDescMcpAction,
    MessageId::ApprovalDescAgent,
    MessageId::ApprovalDescUnknown,
    MessageId::ApprovalImpactSafe,
    MessageId::ApprovalImpactFileWrite,
    MessageId::ApprovalImpactShell,
    MessageId::ApprovalImpactNetwork,
    MessageId::ApprovalImpactMcpRead,
    MessageId::ApprovalImpactMcpAction,
    MessageId::ApprovalImpactAgent,
    MessageId::ApprovalImpactUnknown,
    MessageId::ApprovalLabelCommand,
    MessageId::ApprovalLabelDir,
    MessageId::ApprovalLabelFile,
    MessageId::ApprovalLabelPreview,
    MessageId::ApprovalLabelProposedContent,
    MessageId::ApprovalLabelReplaceThis,
    MessageId::ApprovalLabelWithThis,
    MessageId::ApprovalLabelReplacementContent,
    MessageId::ApprovalLabelPath,
    MessageId::ApprovalLabelTarget,
    MessageId::ApprovalLabelInput,
    MessageId::ApprovalLabelAction,
    MessageId::ApprovalLabelType,
    MessageId::ApprovalLabelPrompt,
    MessageId::ApprovalLabelAbout,
    MessageId::ApprovalLabelImpact,
    MessageId::SetupConstitutionFileNotChecked,
    MessageId::SetupConstitutionFileMissing,
    MessageId::SetupConstitutionFileLoadedSelected,
    MessageId::SetupConstitutionFileLoadedInactive,
    MessageId::SetupConstitutionFileLoadedUnselected,
    MessageId::SetupConstitutionFileEmpty,
    MessageId::SetupConstitutionFileInvalid,
    MessageId::SetupConstitutionFileUnreadable,
    MessageId::SetupConstitutionFilePathError,
    MessageId::SetupExpertOverrideNotChecked,
    MessageId::SetupExpertOverrideMissing,
    MessageId::SetupExpertOverrideActive,
    MessageId::SetupExpertOverrideDisabled,
    MessageId::SetupExpertOverrideEmpty,
    MessageId::SetupExpertOverrideUnreadable,
    MessageId::SetupExpertOverridePathError,
    MessageId::SetupAutonomyUnspecified,
    MessageId::SetupGuidedPurposeCoding,
    MessageId::SetupGuidedPurposeResearch,
    MessageId::SetupGuidedPurposeOperations,
    MessageId::SetupGuidedPurposeMixed,
    MessageId::SetupGuidedPurposeAboutCoding,
    MessageId::SetupGuidedPurposeAboutResearch,
    MessageId::SetupGuidedPurposeAboutOperations,
    MessageId::SetupGuidedPurposeAboutMixed,
    MessageId::SetupGuidedStyleCoding,
    MessageId::SetupGuidedStyleResearch,
    MessageId::SetupGuidedStyleOperations,
    MessageId::SetupGuidedStyleMixed,
    MessageId::SetupGuidedEvidenceAssumptions,
    MessageId::SetupGuidedEvidenceTestsAndReceipts,
    MessageId::SetupGuidedEvidenceReleaseReceipts,
    MessageId::SetupGuidedNotes,
    MessageId::LaunchStartTitle,
    MessageId::LaunchResumeConfirm,
    MessageId::LaunchResumeConfirmTitle,
    MessageId::LaunchResumeConfirmBody,
    MessageId::LaunchResumeConfirmResume,
    MessageId::LaunchResumeConfirmCancel,
    MessageId::LaunchWorkDescription,
    MessageId::LaunchChatDescription,
    MessageId::LaunchWorkspaceGitReady,
    MessageId::LaunchWorkspaceFolderReady,
    MessageId::LaunchProviderConfigured,
    MessageId::LaunchProviderSetupNeeded,
    MessageId::LaunchGroupContinue,
    MessageId::LaunchGroupMore,
    MessageId::LaunchWorkspaceGitShort,
    MessageId::LaunchWorkspaceFolderShort,
    MessageId::LaunchProviderConfiguredShort,
    MessageId::LaunchProviderSetupShort,
    MessageId::LaunchMenuChangelog,
    MessageId::LaunchWorktreePrompt,
    MessageId::LaunchWorktreeNeedsGit,
    MessageId::LaunchWorktreeNameLabel,
    MessageId::LaunchHintMove,
    MessageId::LaunchHintOpen,
    MessageId::LaunchTipFlags,
    MessageId::LaunchSavedSessionSingular,
    MessageId::LaunchSavedSessionsPlural,
    MessageId::LaunchCreatingWorktree,
    MessageId::LaunchWorktreeFailed,
    MessageId::LaunchWorktreeCreated,
    MessageId::LaunchNoSavedSessions,
    MessageId::LaunchNewSession,
    MessageId::LaunchRecentHeading,
    MessageId::LaunchHelpLine,
    MessageId::LaunchSeeAllSessions,
    MessageId::LaunchNoRecentSessions,
    MessageId::LaunchResumeFailed,
    MessageId::LaunchNoModelConnected,
    MessageId::DispatchNotSentNoModel,
    MessageId::DispatchNotSentSaveKey,
    MessageId::DispatchNotSentStatus,
    MessageId::AuthRejectedRecovery,
    MessageId::ProviderKeyRejected,
    MessageId::ProviderKeyForbidden,
    MessageId::ProviderKeyUnreachable,
    MessageId::ProviderKeyCheckFailed,
    MessageId::LocalModelAdopted,
    MessageId::ProviderPickerNotReopened,
    MessageId::LaunchRunCommand,
    MessageId::LaunchMcpConnectedOne,
    MessageId::LaunchMcpConnectedMany,
    MessageId::LaunchMcpNeedsSignInOne,
    MessageId::LaunchMcpNeedsSignInMany,
    MessageId::LaunchMenuNewWorktree,
    MessageId::LaunchMenuResume,
    MessageId::LaunchMenuQuit,
    MessageId::LaunchNoticeClaude,
    MessageId::ReceiptSessionHooks,
    MessageId::PhaseIdle,
    MessageId::PhaseDraft,
    MessageId::PhaseWorking,
    MessageId::PhaseReasoning,
    MessageId::PhaseReading,
    MessageId::PhaseUsingTool,
    MessageId::PhaseSubagents,
    MessageId::PhaseVerifying,
    MessageId::PhaseWaitingOnYou,
    MessageId::PhaseDone,
    MessageId::PhaseFailed,
    MessageId::PhaseFinishing,
    MessageId::ChipModeAct,
    MessageId::ChipModePlan,
    MessageId::ChipModeOperate,
    MessageId::ChipPermissionReadOnly,
    MessageId::ChipPermissionAsk,
    MessageId::ChipPermissionAuto,
    MessageId::ChipPermissionFullAccess,
    MessageId::ChipPermissionNever,
    MessageId::FooterHintKeys,
    MessageId::FooterHintOutput,
    MessageId::FooterHintContext,
    MessageId::InfoLineHelp,
    MessageId::InfoLineContext,
    MessageId::InfoLineTtft,
    MessageId::InfoLinePeak,
    MessageId::InfoLineOffPeak,
    MessageId::InfoLineWhales,
    MessageId::InfoLineAutomation,
    MessageId::InfoLineNotConnected,
    MessageId::InfoLineThinking,
    MessageId::SessionMetricsTurn,
    MessageId::SessionMetricsTurns,
    MessageId::SessionMetricsStep,
    MessageId::SessionMetricsSteps,
    MessageId::SessionMetricsLlm,
    MessageId::SessionMetricsTools,
    MessageId::SessionMetricsTtft,
    MessageId::SessionMetricsTokensPerSecond,
    MessageId::RuntimeStoreRecovered,
    MessageId::ResumeExactSessionHint,
    MessageId::ResumeSavedSessionHint,
    MessageId::GoalProgressLabel,
    MessageId::GoalProgressReceipt,
    MessageId::GoalProgressNow,
    MessageId::GoalProgressNext,
    MessageId::CmdCacheUnpricedNote,
    MessageId::SessionMetricsCache,
    MessageId::SessionMetricsInput,
    MessageId::SessionMetricsStatusLine,
    MessageId::StatusLabelRoute,
    MessageId::StatusLabelDirectory,
    MessageId::StatusLabelProjectDocs,
    MessageId::StatusLabelMode,
    MessageId::StatusLabelSafety,
    MessageId::StatusLabelMcp,
    MessageId::StatusLabelFleet,
    MessageId::StatusLabelContextWindow,
    MessageId::StatusLabelWindowSource,
    MessageId::StatusLabelWindowOverride,
    MessageId::StatusLabelCatalog,
    MessageId::StatusLabelCloudFacts,
    MessageId::StatusLabelSession,
    MessageId::StatusLabelSessionTokens,
    MessageId::StatusLabelSessionCost,
    MessageId::StatusLabelToolOutputs,
    MessageId::StatusRouteSummary,
    MessageId::StatusProjectDocsNone,
    MessageId::StatusPostureSummary,
    MessageId::StatusShellOn,
    MessageId::StatusShellOff,
    MessageId::StatusTrustedWorkspace,
    MessageId::StatusWorkspace,
    MessageId::StatusApprovalAsk,
    MessageId::StatusApprovalAuto,
    MessageId::StatusApprovalFullAccess,
    MessageId::StatusApprovalNever,
    MessageId::StatusMcpConfigured,
    MessageId::StatusFleetDrifted,
    MessageId::StatusModelNotInRoster,
    MessageId::StatusContextUsage,
    MessageId::StatusContextSourceConfigured,
    MessageId::StatusContextSourceConfiguredModel,
    MessageId::StatusContextSourceProviderReported,
    MessageId::StatusContextSourceKimiSafeFloor,
    MessageId::StatusContextSourceCatalog,
    MessageId::StatusContextSourceModelHint,
    MessageId::StatusContextSourceFallback,
    MessageId::StatusWindowOverrideProvider,
    MessageId::StatusWindowOverrideActiveProvider,
    MessageId::StatusSessionNotSaved,
    MessageId::StatusSessionSummary,
    MessageId::StatusSessionTokensSummary,
    MessageId::StatusCacheNotReported,
    MessageId::StatusCacheSummary,
    MessageId::StatusToolRawPressure,
    MessageId::StatusToolCompactReceipts,
    MessageId::StatusToolArtifacts,
    MessageId::StatusToolNone,
    MessageId::StatusSafetyReadOnlyUnenforced,
    MessageId::StatusSafetyReadOnly,
    MessageId::StatusSafetyWorkspaceWriteUnenforcedNetworkOn,
    MessageId::StatusSafetyWorkspaceWriteUnenforcedNetworkOff,
    MessageId::StatusSafetyWorkspaceWriteNetworkOn,
    MessageId::StatusSafetyWorkspaceWriteNetworkOff,
    MessageId::StatusSafetyDisabled,
    MessageId::StatusSafetyDisabledSetuidBlocked,
    MessageId::StatusSafetyDisabledSetuidAllowed,
    MessageId::StatusSafetyExternal,
    MessageId::StatusPointers,
    MessageId::EmptyStateNoGit,
    MessageId::EmptyStateMcpLabel,
    MessageId::EmptyStatePrompt,
    MessageId::SessionsSurfaceTitle,
    MessageId::SessionsPaneTitle,
    MessageId::SessionsHistoryPaneTitle,
    MessageId::SessionsActionResume,
    MessageId::SessionsActionSearch,
    MessageId::SessionsActionSort,
    MessageId::SessionsActionRename,
    MessageId::SessionsActionAllWorkspaces,
    MessageId::SessionsActionDelete,
    MessageId::SessionsActionClose,
    MessageId::SessionsScopeSortHeader,
    MessageId::SessionsEmptyTitle,
    MessageId::SessionsEmptyHint,
    MessageId::SessionsShowingAllWorkspaces,
    MessageId::SessionsScopedToWorkspace,
    MessageId::SessionsNewTitlePrompt,
    MessageId::SessionsDeletePrompt,
    MessageId::SessionsConfirmDelete,
    MessageId::SessionsNewSessionTitle,
    MessageId::SessionsOpenedHistory,
    MessageId::SessionsSortStatus,
    MessageId::SessionsSortRecent,
    MessageId::SessionsSortName,
    MessageId::SessionsSortSize,
    MessageId::SessionsSearchPrompt,
    MessageId::SessionsDeleteFailed,
    MessageId::SessionsDeleteOpenHere,
    MessageId::SessionsDeleteOpenElsewhere,
    MessageId::SessionSaveFailed,
    MessageId::SessionSaveFailedAtExit,
    MessageId::SessionsDeleted,
    MessageId::SessionsNoSelection,
    MessageId::SessionsTitleLength,
    MessageId::SessionsOpenFailed,
    MessageId::SessionsLoadFailed,
    MessageId::SessionsResumed,
    MessageId::SessionsRenameFailed,
    MessageId::SessionsRenamed,
    MessageId::SessionsRailTitle,
    MessageId::SessionsRailEmpty,
    MessageId::SessionsRailBrowseAll,
    MessageId::SessionsRailShowingCount,
    MessageId::SessionsRailUnavailable,
    MessageId::SessionsActionArchive,
    MessageId::SessionsActionShowArchived,
    MessageId::SessionsArchived,
    MessageId::SessionsRestored,
    MessageId::SessionsArchiveFailed,
    MessageId::SessionsShowingArchived,
    MessageId::SessionsHidingArchived,
    MessageId::SessionsArchivedCompact,
    MessageId::SessionsNoResults,
    MessageId::SessionsDirectoryFailed,
    MessageId::SessionsPreviewFailed,
    MessageId::SessionsDeleteCancelled,
    MessageId::SessionsRenameCancelled,
    MessageId::SessionsShowingRange,
    MessageId::SessionsMessageCountCompact,
    MessageId::SessionsForkCompact,
    MessageId::SessionsCurrentCompact,
    MessageId::SessionsUnknownMode,
    MessageId::SessionsPreviewTitle,
    MessageId::SessionsPreviewId,
    MessageId::SessionsPreviewUpdated,
    MessageId::SessionsPreviewMessagesModel,
    MessageId::SessionsPreviewMode,
    MessageId::SessionsToolCall,
    MessageId::SessionsToolError,
    MessageId::SessionsToolResult,
    MessageId::SessionsServerTool,
    MessageId::SessionsImage,
    MessageId::SessionsTimeJustNow,
    MessageId::SessionsTimeMinutesAgo,
    MessageId::SessionsTimeHoursAgo,
    MessageId::SessionsTimeDaysAgo,
    MessageId::CtxInspRowSystemPrompt,
    MessageId::CtxInspRowMessages,
    MessageId::CtxInspRowFree,
    MessageId::CtxInspFreeTokensDetail,
    MessageId::CtxInspDrillTitle,
    MessageId::CtxInspSurfaceTitle,
    MessageId::CtxInspActionSelect,
    MessageId::CtxInspActionDrillDown,
    MessageId::CtxInspActionClose,
    MessageId::CtxInspUsedTokens,
    MessageId::CtxInspAutoCompactAt,
    MessageId::CtxInspRowTokens,
    MessageId::CtxInspRowCompaction,
    MessageId::CtxInspRowAnchors,
    MessageId::CtxInspCompactionNever,
    MessageId::CtxInspCompactionDetail,
    MessageId::CtxInspCompactionRestored,
    MessageId::CtxInspCompactionPathSummary,
    MessageId::CtxInspCompactionPathPrune,
    MessageId::CtxInspCompactionAssistantKept,
    MessageId::CtxInspAnchorsNone,
    MessageId::CtxInspAnchorsPresent,
    MessageId::RouteSurfaceTitle,
    MessageId::RouteBrowseCatalog,
    MessageId::RouteActionType,
    MessageId::RouteActionSearchAnyModel,
    MessageId::RoutePanelHeader,
    MessageId::RouteProviderLabel,
    MessageId::RouteModelFirstAtomic,
    MessageId::PickerActionMove,
    MessageId::PickerActionSwitch,
    MessageId::PickerActionApply,
    MessageId::PickerActionAssignRoute,
    MessageId::FleetRoutePickUnavailable,
    MessageId::FleetRouteSaved,
    MessageId::FleetRouteInherited,
    MessageId::FleetRouteNotInCatalog,
    MessageId::PickerActionSetStartupDefault,
    MessageId::PickerActionPin,
    MessageId::PickerActionFleet,
    MessageId::PickerActionCancel,
    MessageId::PickerActionClear,
    MessageId::PickerActionClearSearch,
    MessageId::PickerActionBrowseAll,
    MessageId::PickerActionCustom,
    MessageId::PickerActionJump,
    MessageId::PickerActionEditKey,
    MessageId::PickerActionModels,
    MessageId::PickerActionUnavailable,
    MessageId::PickerActionSetKey,
    MessageId::PickerActionConfigured,
    MessageId::RouteNoModels,
    MessageId::RouteNoModelMatch,
    MessageId::ProviderNoMatchesTitle,
    MessageId::ProviderNoMatchesHint,
    MessageId::ProviderNoConfiguredTitle,
    MessageId::ProviderNoConfiguredHint,
    MessageId::ProviderNoCatalogModels,
    MessageId::ProviderExternalActionRevoke,
    MessageId::ProviderExternalActionChoices,
    MessageId::ProviderExternalActionReuseGrok,
    MessageId::ProviderExternalHintCodexReview,
    MessageId::ProviderExternalHintXaiReview,
    MessageId::ProviderExternalHintXaiApiKey,
    MessageId::XaiAuthChoiceTitle,
    MessageId::XaiAuthChoiceIntro,
    MessageId::XaiAuthChoiceApiKeyOption,
    MessageId::XaiAuthChoiceDeviceOAuthOption,
    MessageId::OrcarouterAuthChoiceTitle,
    MessageId::OrcarouterAuthChoiceIntro,
    MessageId::OrcarouterAuthChoiceApiKeyOption,
    MessageId::OrcarouterAuthChoicePkceOption,
    MessageId::ChatgptAuthChoiceTitle,
    MessageId::ChatgptAuthChoiceIntro,
    MessageId::ChatgptAuthChoicePkceOption,
    MessageId::ChatgptAuthChoiceImportOption,
    MessageId::ProviderExternalHintChatgptReview,
    MessageId::ProviderExternalActionReuseCodex,
    MessageId::ProviderExternalDetailScope,
    MessageId::ProviderExternalDormant,
    MessageId::ProviderExternalOwnerPath,
    MessageId::ProviderExternalPinnedPathWarning,
    MessageId::ToolProjectionWarning,
    MessageId::SnapshotsDisabledTooLarge,
    MessageId::SnapshotsDisabledTooManyFiles,
    MessageId::SnapshotsDisabledUnsafeLocation,
    MessageId::SnapshotsHistoryRepaired,
    MessageId::SnapshotsFailing,
    MessageId::SessionIdDivergedNotice,
    MessageId::RuntimeStoreUnreadableNotice,
    MessageId::RuntimeStoreUnwritableNotice,
    MessageId::ProviderExternalSemanticsRevoke,
    MessageId::ProviderExternalRevoke,
    MessageId::ProviderExternalChoiceTitle,
    MessageId::ProviderExternalActionChoose,
    MessageId::ProviderExternalChoiceIntro,
    MessageId::ProviderExternalDisabledLabel,
    MessageId::ProviderExternalDisabledDetail,
    MessageId::ProviderExternalReadOnlyLabel,
    MessageId::ProviderExternalReadOnlyDetail,
    MessageId::ProviderExternalReadOnlySemantics,
    MessageId::ProviderExternalManagedLabel,
    MessageId::ProviderExternalManagedDetail,
    MessageId::ProviderExternalConfirmTitle,
    MessageId::ProviderExternalActionGrant,
    MessageId::ProviderExternalOwnerLabel,
    MessageId::ProviderExternalExactPathLabel,
    MessageId::ProviderExternalSemanticsLabel,
    MessageId::ProviderExternalRejectUnsafe,
    MessageId::ProviderExternalRevokeLabel,
    MessageId::ProviderExternalRouteLabel,
    MessageId::ProviderExternalCustodyLine,
    MessageId::ProviderExternalBillingLine,
    MessageId::ProviderExternalRevokeScope,
    MessageId::ProviderExternalOwnerOnly,
    MessageId::ProviderExternalPinnedPathChanged,
    MessageId::ProviderExternalRevokeConfirmTitle,
    MessageId::ProviderExternalGrantedToast,
    MessageId::ProviderExternalSaveFailedToast,
    MessageId::ProviderExternalRevokedToast,
    MessageId::ProviderExternalRevokeFailedToast,
    MessageId::ThemeSurfaceTitle,
    MessageId::FleetRosterHeaderLabel,
    MessageId::FleetRosterTabRoster,
    MessageId::FleetRosterTabSetup,
    MessageId::FleetRosterWorkers,
    MessageId::FleetRosterMembersCount,
    MessageId::FleetRosterOperatorFirst,
    MessageId::FleetRosterOperatorRow,
    MessageId::FleetRosterShadowBadgeProjectOverride,
    MessageId::FleetRosterShadowBadgePersonalIgnored,
    MessageId::FleetRosterShadowBadgePersonalOverride,
    MessageId::FleetRosterShadowBadgeConfigOverride,
    MessageId::FleetRosterLayersLabel,
    MessageId::FleetRosterLayerWins,
    MessageId::FleetRosterLayerIgnored,
    MessageId::FleetReadyNotice,
    MessageId::FleetProfileIdentityVerifyFailed,
    MessageId::FleetProfileIdConflict,
    MessageId::FleetProfileProviderUnconfigured,
    MessageId::FleetModelAdded,
    MessageId::FleetModelAddedAs,
    MessageId::FleetModelAddedCreatedNote,
    MessageId::FleetModelAddedSelectedNote,
    MessageId::FleetModelRemoved,
    MessageId::FleetModelRemovedRoles,
    MessageId::FleetModelUnchanged,
    MessageId::FleetModelReasonOperatorRoute,
    MessageId::FleetModelReasonAlreadyPresent,
    MessageId::FleetModelErrorNeedsRoute,
    MessageId::FleetModelErrorNeedsRole,
    MessageId::FleetReviewWorkspaceLimits,
    MessageId::FleetReviewPolicy,
    MessageId::FleetModelErrorNoSelection,
    MessageId::FleetModelErrorOperatorRoute,
    MessageId::FleetModelErrorNotInFleet,
    MessageId::FleetModelsEmpty,
    MessageId::FleetModelsHeader,
    MessageId::FleetModelsBroken,
    MessageId::FleetModelsFooter,
    MessageId::FleetModelsFactPrice,
    MessageId::FleetModelsFactContext,
    MessageId::FleetModelsFactTools,
    MessageId::FleetAddUsage,
    MessageId::FleetAddProviderUnconfigured,
    MessageId::FleetAddModelNotServed,
    MessageId::FleetAddFailed,
    MessageId::FleetRemoveUsage,
    MessageId::FleetRemoveFailed,
    MessageId::FleetToggleFailed,
    MessageId::FleetDestStepTitle,
    MessageId::FleetDestStepSubtitle,
    MessageId::FleetDestProjectLabel,
    MessageId::FleetDestPersonalLabel,
    MessageId::FleetDestProjectSummary,
    MessageId::FleetDestPersonalSummary,
    MessageId::FleetDestProjectDescription,
    MessageId::FleetDestPersonalDescription,
    MessageId::FleetDestPathLine,
    MessageId::FleetDestUnavailable,
    MessageId::FleetDestReasonNoProjectConfig,
    MessageId::FleetDestReasonWorkspaceMissing,
    MessageId::FleetDestReasonHomeUnavailable,
    MessageId::FleetDestWillReplace,
    MessageId::FleetDestOverridesProject,
    MessageId::FleetDestOverridesPersonal,
    MessageId::FleetDestOverridesBuiltIn,
    MessageId::FleetSavesToChip,
    MessageId::FleetSavesToUndecided,
    MessageId::FleetActionSaveProject,
    MessageId::FleetActionSavePersonal,
    MessageId::FleetActionReplaceProject,
    MessageId::FleetActionReplacePersonal,
    MessageId::FleetActionConfirmReplace,
    MessageId::FleetActionChangeDestination,
    MessageId::FleetActionBack,
    MessageId::FleetReviewSavesTo,
    MessageId::FleetModelRowBlockedNotice,
    MessageId::FleetDestProjectDisabledSave,
    MessageId::WorkflowStatusWaiting,
    MessageId::WorkflowStatusDegraded,
    MessageId::WorkflowRunFailedToast,
    MessageId::WorkflowDebrief,
    MessageId::WorkflowDispatchFailureLine,
    MessageId::WorkflowDispatchFailuresOmitted,
    MessageId::WorkflowDispatchFallbackTask,
    MessageId::WorkflowTranscriptDetails,
    MessageId::WorkflowReceiptRole,
    MessageId::WorkflowReceiptReasoning,
    MessageId::WorkflowReceiptVia,
    MessageId::WorkflowReceiptTokens,
    MessageId::WorkflowReceiptTools,
    MessageId::WorkflowReceiptDuration,
    MessageId::WorkflowReceiptUnknown,
    MessageId::WorkflowReceiptProviderReported,
    MessageId::WorkflowReceiptEstimated,
    MessageId::SidebarTasksLabel,
    MessageId::TaskOwnershipUnverified,
    MessageId::TaskInventoryUnavailable,
    MessageId::SidebarTodoLabel,
    MessageId::SidebarStopControl,
    MessageId::SidebarDestructiveArmed,
    MessageId::WorkSurfaceTodoProgress,
    MessageId::WorkSurfaceStopConfirmHint,
    MessageId::TerminalDockStarting,
    MessageId::TerminalDockControls,
    MessageId::TerminalDockEmpty,
    MessageId::TerminalDockUnsupported,
    MessageId::TerminalDockLost,
    MessageId::TerminalDockDropped,
    MessageId::TerminalDockError,
    MessageId::TerminalDockPasteRefused,
    MessageId::TerminalDockExited,
    MessageId::TerminalDockLive,
    MessageId::CoordinationWorkTitle,
    MessageId::CoordinationSummaryDecisions,
    MessageId::CoordinationSummaryContentions,
    MessageId::CoordinationSummaryReconciled,
    MessageId::CoordinationSchema,
    MessageId::CoordinationSequence,
    MessageId::CoordinationPerSectionLimit,
    MessageId::CoordinationDecisionsHeading,
    MessageId::CoordinationNone,
    MessageId::CoordinationNoneValue,
    MessageId::CoordinationStatus,
    MessageId::CoordinationOwner,
    MessageId::CoordinationVersion,
    MessageId::CoordinationWriteClaimsHeading,
    MessageId::CoordinationIsolated,
    MessageId::CoordinationSharedWorkspace,
    MessageId::CoordinationPaths,
    MessageId::CoordinationContracts,
    MessageId::CoordinationContentionsHeading,
    MessageId::CoordinationClaimant,
    MessageId::CoordinationDisposition,
    MessageId::CoordinationNeutralReconciliationHeading,
    MessageId::CoordinationCandidates,
    MessageId::CoordinationRetry,
    MessageId::CoordinationReviewer,
    MessageId::CoordinationVerifier,
    MessageId::CoordinationVerification,
    MessageId::CoordinationContextProjectionsHeading,
    MessageId::CoordinationContextDecisions,
    MessageId::CoordinationBytes,
    MessageId::CoordinationDeduplicated,
    MessageId::CoordinationOmitted,
    MessageId::CoordinationActiveHotPathsHeading,
    MessageId::CoordinationActiveClaims,
    MessageId::CoordinationMetricsNoteHeading,
    MessageId::CoordinationMetricsNoAuthoritativeSource,
    MessageId::CoordinationStatusProposed,
    MessageId::CoordinationStatusAccepted,
    MessageId::CoordinationStatusSuperseded,
    MessageId::ComposerSlashMenuHint,
    MessageId::ApprovalRepoLawBadge,
    MessageId::ApprovalRepoLawTitle,
    MessageId::ApprovalRepoLawWarning,
    MessageId::ApprovalRepoLawRuleLabel,
    MessageId::FilePickerMatchSingular,
    MessageId::FilePickerMatchesPlural,
    MessageId::FilePickerScanning,
    MessageId::BehavioralTipPlanning,
    MessageId::BehavioralTipBackgroundReceipt,
    MessageId::BehavioralTipClearedInput,
    MessageId::BehavioralTipMcpValidation,
    MessageId::BehavioralTipRepeatedCommand,
    MessageId::BehavioralTipDurableStateWritten,
    MessageId::BehavioralTipTodoWrite,
    MessageId::ConfigLabelContextualTips,
    MessageId::ConfigHintContextualTips,
    MessageId::ContextualTipsNotSaved,
    MessageId::SettingLockedDuringTurn,
    MessageId::SettingSubjectMode,
    MessageId::SettingSubjectThinking,
    MessageId::SettingSubjectModel,
    MessageId::SettingSubjectModelAndThinking,
    MessageId::SettingSubjectProvider,
    MessageId::SettingSubjectPermissions,
    MessageId::ThinkingControlledByAutoRouting,
    MessageId::SavedAsStartupDefault,
    MessageId::ModeAlreadyActiveSavedAsDefault,
    MessageId::StartupDefaultNotSaved,
    MessageId::RouteSaveNothingPending,
    MessageId::RouteSaveNoTeamSelected,
    MessageId::RouteSaveTeamUpdated,
    MessageId::RouteSaveTeamUpdateFailed,
    MessageId::RouteSaveTeamUpdateFailedMoved,
    MessageId::RouteSaveTeamCreateFailed,
    MessageId::RouteSaveSelectedNote,
    MessageId::RouteSaveSelectionFailedNote,
    MessageId::RouteSaveSavedAsNewTeam,
    MessageId::RouteSaveFailed,
    MessageId::RouteSaveRouteNoLongerActive,
    MessageId::RouteSaveRememberedDefault,
    MessageId::StartupDefaultSubjectMode,
    MessageId::StartupDefaultSubjectThinking,
    MessageId::StartupDefaultSubjectModel,
    MessageId::StartupDefaultSubjectAll,
    MessageId::AutomationUsage,
    MessageId::AutomationManagerUnavailable,
    MessageId::AutomationListFailed,
    MessageId::AutomationActionFailed,
    MessageId::AutomationEmpty,
    MessageId::AutomationListHeading,
    MessageId::AutomationScopeNote,
    MessageId::AutomationNoun,
    MessageId::AutomationStatusLabel,
    MessageId::AutomationStatusActive,
    MessageId::AutomationStatusPaused,
    MessageId::AutomationRunStatusQueued,
    MessageId::AutomationRunStatusRunning,
    MessageId::AutomationRunStatusCompleted,
    MessageId::AutomationRunStatusFailed,
    MessageId::AutomationRunStatusCanceled,
    MessageId::AutomationActionInspect,
    MessageId::AutomationActionPause,
    MessageId::AutomationActionResume,
    MessageId::AutomationActionDelete,
    MessageId::AutomationActionRun,
    MessageId::AutomationActionCancel,
    MessageId::AutomationActionPaused,
    MessageId::AutomationActionResumed,
    MessageId::AutomationNextLabel,
    MessageId::AutomationNameLabel,
    MessageId::AutomationEditorNew,
    MessageId::AutomationEditorEdit,
    MessageId::AutomationEditorSchedule,
    MessageId::AutomationEditorDaily,
    MessageId::AutomationEditorWeekly,
    MessageId::AutomationEditorHourly,
    MessageId::AutomationEditorOnce,
    MessageId::AutomationEditorCustom,
    MessageId::AutomationEditorTime,
    MessageId::AutomationEditorDate,
    MessageId::AutomationEditorDays,
    MessageId::AutomationEditorLocalTime,
    MessageId::AutomationEditorProvider,
    MessageId::AutomationEditorDefaultModel,
    MessageId::AutomationEditorInheritedProvider,
    MessageId::AutomationEditorEnabled,
    MessageId::AutomationEditorControls,
    MessageId::AutomationEditorPromptControls,
    MessageId::AutomationEditorSaveFailed,
    MessageId::AutomationEditorBusy,
    MessageId::AutomationEditorSaved,
    MessageId::AutomationEditorPreviewUnavailable,
    MessageId::AutomationEditorConflict,
    MessageId::AutomationEditorInvalidWorkspace,
    MessageId::AutomationEditorModelControls,
    MessageId::AutomationEditorPausedPreview,
    MessageId::AutomationEditorInvalidSchedule,
    MessageId::AutomationEditorMonday,
    MessageId::AutomationEditorTuesday,
    MessageId::AutomationEditorWednesday,
    MessageId::AutomationEditorThursday,
    MessageId::AutomationEditorFriday,
    MessageId::AutomationEditorSaturday,
    MessageId::AutomationEditorSunday,
    MessageId::AutomationPromptLabel,
    MessageId::AutomationCwdLabel,
    MessageId::AutomationModeLabel,
    MessageId::AutomationAllowShellLabel,
    MessageId::AutomationTrustModeLabel,
    MessageId::AutomationAutoApproveLabel,
    MessageId::AutomationRruleLabel,
    MessageId::AutomationDeliveryLabel,
    MessageId::AutomationLastLabel,
    MessageId::AutomationRecentRunsLabel,
    MessageId::AutomationNoRuns,
    MessageId::AutomationRunsUnavailable,
    MessageId::AutomationTaskLabel,
    MessageId::AutomationDeletePreview,
    MessageId::AutomationDeleteConfirmationStale,
    MessageId::AutomationBandScheduled,
    MessageId::AutomationReceiptFired,
    MessageId::AutomationReceiptStarted,
    MessageId::AutomationReceiptCompleted,
    MessageId::AutomationReceiptCoalesced,
    MessageId::AutomationReceiptMissed,
    MessageId::AutomationReceiptExpired,
    MessageId::AutomationReceiptDeleted,
    MessageId::AutomationRunLabel,
    MessageId::AutomationDeletedRunsDetail,
    MessageId::WhaleStateResting,
    MessageId::WhaleStateThinking,
    MessageId::WhaleStateWorking,
    MessageId::WhaleStateWaiting,
    MessageId::WhaleStateBlocked,
    MessageId::WhaleStateOffline,
    MessageId::AgentStatusParked,
    MessageId::AgentStatusParkedRecovery,
    MessageId::WhaleAnimalScout,
    MessageId::WhaleAnimalPatch,
    MessageId::WhaleAnimalHarbor,
    MessageId::WhaleAnimalEcho,
    MessageId::WhaleAnimalKeel,
    MessageId::WhaleAnimalLantern,
    MessageId::WhaleAnimalPlain,
    MessageId::WhaleJobScout,
    MessageId::WhaleJobPatch,
    MessageId::WhaleJobHarbor,
    MessageId::WhaleJobEcho,
    MessageId::WhaleJobKeel,
    MessageId::WhaleJobLantern,
    MessageId::WhaleJobPlain,
    MessageId::AgentFocusOpened,
    MessageId::AgentFocusClosed,
    MessageId::AgentFocusBanner,
    MessageId::AgentFocusPosture,
    MessageId::AgentFocusPostureWrites,
    MessageId::AgentFocusPostureReadOnly,
    MessageId::AgentFocusPostureNetwork,
    MessageId::AgentFocusPostureNoNetwork,
    MessageId::AgentFocusPostureShellFull,
    MessageId::AgentFocusPostureShellReadOnly,
    MessageId::AgentFocusPostureShellNone,
    MessageId::AgentFocusComposerChip,
    MessageId::AgentFocusPlaceholder,
    MessageId::AgentFocusNoTranscript,
    MessageId::AgentFocusOmitted,
    MessageId::AgentFocusResult,
    MessageId::AgentFocusStopReason,
    MessageId::AgentFocusFollowUpDelivered,
    MessageId::AgentFocusFollowUpQueued,
    MessageId::AgentFocusFollowUpContinued,
    MessageId::AgentFocusFollowUpFailed,
    MessageId::FooterHintForAgents,
    MessageId::FooterHintToManage,
    MessageId::AgentRailQueuedCount,
    MessageId::WorkbarSoFar,
    MessageId::WorkbarLargeWorkflow,
    MessageId::WorkbarMoreRuns,
    MessageId::WorkflowLineStarted,
    MessageId::WorkflowLineFinished,
    MessageId::WorkflowLineFinishedWithGaps,
    MessageId::WorkflowLineFailed,
    MessageId::WorkflowLineStopped,
    MessageId::WorkflowLineAgents,
    MessageId::PickerActionTestConnection,
    MessageId::ProviderCustomFormBaseUrl,
    MessageId::ProviderCustomFormModel,
    MessageId::ProviderCustomFormHint,
    MessageId::ProviderConnectionChecked,
    MessageId::ProviderConnectionCheckedPickModel,
    MessageId::ProviderTestConnectionNeedKey,
    MessageId::ProviderTestConnectionFailed,
    MessageId::ProviderTestConnectionNoEndpoint,
    MessageId::ComposerPlaceholderFollowUp,
    MessageId::ComposerPlaceholderSendNow,
    MessageId::ComposerHintSendWithQueue,
    MessageId::ComposerHintQueue,
    MessageId::ComposerHintQueueWithCount,
    MessageId::ComposerHintOfflineQueue,
    MessageId::ComposerHintOfflineConnect,
    MessageId::ComposerHintSendNow,
    MessageId::ComposerHintSendIntoTurn,
    MessageId::PendingSendingIntoTurnPrefix,
    MessageId::PendingCouldNotSendIntoTurnPrefix,
    MessageId::PendingEditingFollowUpPrefix,
    MessageId::PendingQueuedOnePrefix,
    MessageId::PendingQueuedManyPrefix,
    MessageId::PendingSendNowControls,
    MessageId::PendingSendNowDropControls,
    MessageId::PendingEscRestore,
    MessageId::PendingQueuedFollowUpPrefix,
    MessageId::PendingInputsHeader,
    MessageId::PendingContextHeader,
    MessageId::ToastQueuedFollowUp,
    MessageId::ToastQueuedFollowUpCount,
    MessageId::ToastQueuedOffline,
    MessageId::ToastSentIntoTurn,
    MessageId::ToastCouldNotSendIntoTurn,
    MessageId::ToastHookBlockedFollowUp,
    MessageId::ToastOfflineQueuedCount,
    MessageId::OperateBoardHeader,
    MessageId::OperateBoardBurnObserved,
    MessageId::OperateBoardBurnNoCap,
    MessageId::OperateBoardDirectionEmpty,
    MessageId::OperateBoardDirectionLine,
    MessageId::OperateBoardPlanMissing,
    MessageId::OperateBoardPlanHeader,
    MessageId::OperateBoardGantt,
    MessageId::ConfigCategoryAppearance,
    MessageId::ConfigCategoryModelsProviders,
    MessageId::ConfigCategoryFleet,
    MessageId::ConfigCategoryWork,
    MessageId::ConfigCategoryToolsMcp,
    MessageId::ConfigCategoryTrust,
    MessageId::ConfigCategoryMotion,
    MessageId::ConfigCategoryAdvanced,
    MessageId::ConfigFactCurrent,
    MessageId::ConfigFactSaved,
    MessageId::ConfigFactStartup,
    MessageId::ConfigFactSource,
    MessageId::ConfigFactScope,
    MessageId::ConfigFactApply,
    MessageId::ConfigFactKind,
    MessageId::ConfigFactAvailable,
    MessageId::ConfigFactObserved,
    MessageId::ConfigFactOpens,
    MessageId::ConfigLaneUnobserved,
    MessageId::ConfigSourceSession,
    MessageId::ConfigSourceUserSettings,
    MessageId::ConfigSourceConfig,
    MessageId::ConfigSourceManaged,
    MessageId::ConfigApplyEffectiveNow,
    MessageId::ConfigApplyOnSave,
    MessageId::ConfigApplyNextSession,
    MessageId::ConfigApplyRestart,
    MessageId::ConfigApplyReadOnly,
    MessageId::ConfigApplyReload,
    MessageId::ConfigApplyUiNowEngineRestart,
    MessageId::ConfigKindToggle,
    MessageId::ConfigKindChoice,
    MessageId::ConfigKindNumber,
    MessageId::ConfigKindText,
    MessageId::ConfigKindAction,
    MessageId::ConfigKindReadOnly,
    MessageId::ConfigRowActionNote,
    MessageId::ConfigRowDiagnosticNote,
    MessageId::ConfigNavHint,
    MessageId::ConfigActivateAgain,
    MessageId::ConfigSearchLabel,
    MessageId::ConfigEditChooseLabel,
    MessageId::ConfigChoiceFooter,
    MessageId::ConfigChoiceFooterCompact,
    MessageId::ConfigEditorApply,
    MessageId::ConfigEditorCancel,
    MessageId::ConfigLaneUnavailable,
    MessageId::ConfigSourceEnvironment,
    MessageId::ConfigSourceTerminal,
    MessageId::ConfigDescriptionDefault,
    MessageId::ConfigValueOn,
    MessageId::ConfigValueOff,
    MessageId::ConfigValueProviderDefault,
    MessageId::ConfigChoiceAsk,
    MessageId::ConfigChoiceAutoReview,
    MessageId::ConfigChoiceUseTuiDefault,
    MessageId::ConfigChoiceFullAccess,
    MessageId::ConfigChoiceNever,
    MessageId::ConfigChoiceModeAct,
    MessageId::ConfigChoiceModePlan,
    MessageId::ConfigChoiceModeOperate,
    MessageId::ConfigChoicePlacementTop,
    MessageId::ConfigChoicePlacementBottom,
    MessageId::ConfigChoicePlacementLeft,
    MessageId::ConfigChoicePlacementRight,
    MessageId::ConfigChoiceRailTasks,
    MessageId::ConfigChoiceRailAgents,
    MessageId::ConfigChoiceRailContext,
    MessageId::ConfigChoiceStatusCw,
    MessageId::ConfigChoiceStatusDots,
    MessageId::ConfigChoiceDiffFull,
    MessageId::ConfigChoiceDiffSummary,
    MessageId::ConfigChoiceDetailAsk,
    MessageId::ConfigChoiceDetailAutoReview,
    MessageId::ConfigChoiceDetailUseTuiDefault,
    MessageId::ConfigChoiceDetailFullAccess,
    MessageId::ConfigChoiceDetailNever,
    MessageId::ConfigChoiceDetailModeAgent,
    MessageId::ConfigChoiceDetailModePlan,
    MessageId::ConfigChoiceDetailModeOperate,
    MessageId::ConfigChoiceDetailPlacementTop,
    MessageId::ConfigChoiceDetailPlacementBottom,
    MessageId::ConfigChoiceDetailPlacementLeft,
    MessageId::ConfigChoiceDetailPlacementRight,
    MessageId::ConfigChoiceDetailPlacementOff,
    MessageId::ConfigChoiceDetailRailTasks,
    MessageId::ConfigChoiceDetailRailAgents,
    MessageId::ConfigChoiceDetailRailContext,
    MessageId::ConfigChoiceDetailLowMotionOn,
    MessageId::ConfigChoiceDetailLowMotionOff,
    MessageId::ConfigChoiceDetailFancyOn,
    MessageId::ConfigChoiceDetailFancyOff,
    MessageId::ConfigChoiceDetailShowThinkingOn,
    MessageId::ConfigChoiceDetailShowThinkingOff,
    MessageId::ConfigChoiceDetailThinkingHighlightOn,
    MessageId::ConfigChoiceDetailThinkingHighlightOff,
    MessageId::ConfigHintModel,
    MessageId::ConfigHintFastModel,
    MessageId::ConfigHintProvider,
    MessageId::ConfigHintApprovalMode,
    MessageId::ConfigHintPermissionPosture,
    MessageId::ConfigHintApprovalPolicy,
    MessageId::ConfigHintManagedApprovalPolicy,
    MessageId::ConfigHintManagedAllowShell,
    MessageId::ConfigHintAllowShell,
    MessageId::ConfigHintComposerMultilineMode,
    MessageId::ConfigHintBooleanValues,
    MessageId::ConfigHintDensity,
    MessageId::ConfigHintInlineDiffs,
    MessageId::ConfigHintToolCollapse,
    MessageId::ConfigHintBackgroundColor,
    MessageId::ConfigHintWorkSurfacePlacement,
    MessageId::ConfigHintRailPanel,
    MessageId::ConfigHintWorkSurfaceTopHeight,
    MessageId::ConfigHintWorkSurfaceSideWidth,
    MessageId::ConfigHintBaseUrl,
    MessageId::ConfigHintContextWindow,
    MessageId::ConfigHintEffectiveContextWindow,
    MessageId::ConfigHintCostCurrency,
    MessageId::ConfigHintCalmMode,
    MessageId::ConfigHintLowMotion,
    MessageId::ConfigHintFancyAnimations,
    MessageId::ConfigHintShowThinking,
    MessageId::ConfigHintThinkingDefaultExpanded,
    MessageId::ConfigHintThinkingPreviewLines,
    MessageId::ConfigHintHelpExpandGroups,
    MessageId::ConfigHintPinLastPrompt,
    MessageId::ConfigHintThinkingHighlight,
    MessageId::ConfigHintSynchronizedOutput,
    MessageId::ConfigHintDefaultMode,
    MessageId::ConfigHintMaxHistory,
    MessageId::ConfigHintAutoCompactThreshold,
    MessageId::ConfigHintDefaultModel,
    MessageId::ConfigHintReasoningEffort,
    MessageId::ConfigHintMcpOpen,
    MessageId::ConfigHintMcpReconnect,
    MessageId::ConfigHintMcpDiagnose,
    MessageId::ConfigHintPluginsOpen,
    MessageId::ConfigHintMcpConfigPath,
    MessageId::ConfigHintFleetMaxSpawnDepth,
    MessageId::ConfigHintFeatureSubagents,
    MessageId::ConfigHintFeatureWebSearch,
    MessageId::ConfigHintFeatureApplyPatch,
    MessageId::ConfigHintFeatureMcp,
    MessageId::ConfigHintFeatureExecPolicy,
    MessageId::ConfigHintFeatureVisionModel,
    MessageId::ConfigHintGoalCommand,
    MessageId::ConfigHintWorkflow,
    MessageId::SelectionCopiedAsMarkdown,
    MessageId::McpShowCachedWhileTurnRuns,
    MessageId::McpShowUnavailableWhileTurnRuns,
    MessageId::McpLivePoolRefreshDeferredWhileTurnRuns,
    MessageId::McpRetryDeferredWhileTurnRuns,
    MessageId::McpRetryStarted,
    MessageId::McpRetryConnected,
    MessageId::McpRetryNeedsLogin,
    MessageId::McpRetryFailed,
    MessageId::WorkflowCountRunning,
    MessageId::WorkflowCountDone,
    MessageId::WorkflowCountFailed,
    MessageId::WorkflowCountCancelled,
    MessageId::WorkflowNoTasksYet,
    MessageId::WorkflowSettledOfTotal,
    MessageId::WorkflowPhaseCountOne,
    MessageId::WorkflowPhaseCountMany,
    MessageId::WorkflowPhaseOrdinal,
    MessageId::ModelPickerPinned,
    MessageId::ModelPickerUnpinned,
    MessageId::ModelPickerPinFailed,
    MessageId::ModelPickerPinOrderUpdated,
    MessageId::ModelPickerPinReorderFailed,
    MessageId::ModelPickerReadinessRefreshed,
    MessageId::ModelPickerOpenToRefresh,
    MessageId::ModelPickerPinnedChip,
    MessageId::AuthSignedInAs,
    MessageId::AuthSignedInWithoutEmail,
    MessageId::AuthReplacedPreviousSignInAs,
    MessageId::AuthReplacedPreviousSignIn,
    MessageId::AuthSameAccountAsBefore,
    MessageId::AuthEnvTokenOutranksSignIn,
    MessageId::PinnedPromptJumpToMessage,
    MessageId::PlanHandoffHeader,
    MessageId::PlanHandoffQuestion,
    MessageId::PlanHandoffWorkAsk,
    MessageId::PlanHandoffWorkAskDetail,
    MessageId::PlanHandoffWorkAuto,
    MessageId::PlanHandoffWorkAutoDetail,
    MessageId::PlanHandoffKeepPlanning,
    MessageId::PlanHandoffKeepPlanningDetail,
    MessageId::PluginOAuthBrowser,
    MessageId::PluginOAuthReady,
    MessageId::PluginOAuthLocalLogout,
    MessageId::PlanHandoffProceed,
];

pub fn tr(locale: Locale, id: MessageId) -> Cow<'static, str> {
    rust_i18n::t!(format!("{id:?}"), locale = locale.tag())
}

/// Resolve a message by its registry key — the `MessageId` variant name.
///
/// The settings schema (`codewhale_config::SETTINGS_SCHEMA`) names the string
/// a setting shows rather than carrying its prose, so it needs to resolve a
/// key it holds as a `&str`. An unknown key returns the key itself, which the
/// schema binding test in `crate::tui::views` fails on.
pub fn tr_key(locale: Locale, key: &'static str) -> Cow<'static, str> {
    rust_i18n::t!(key, locale = locale.tag())
}

pub fn thinking_translation_placeholder(locale: Locale) -> &'static str {
    match locale {
        Locale::En => "Thinking; translating when complete...",
        Locale::Ja => "思考中です。完了後に日本語へ翻訳します...",
        Locale::ZhHans => "正在思考，完成后翻译为简体中文...",
        Locale::ZhHant => "正在思考，完成後翻譯為繁體中文...",
        Locale::PtBr => "Pensando; traduzindo ao concluir...",
        Locale::Es419 => "Pensando; traduciendo al finalizar...",
        Locale::Vi => "Đang suy nghĩ; sẽ dịch sau khi hoàn thành...",
        Locale::Ko => "생각하는 중입니다. 완료되면 번역합니다...",
        Locale::Ca => "S'està pensant; es traduirà en acabar...",
        Locale::De => "Denkt nach; Übersetzung folgt nach Abschluss...",
        Locale::Fr => "Réflexion en cours ; traduction à la fin...",
        Locale::Id => "Sedang berpikir; akan diterjemahkan setelah selesai...",
        Locale::Hi => "सोच रहा है; पूरा होने पर अनुवाद होगा...",
        Locale::Ru => "Идут размышления; перевод будет после завершения...",
        Locale::Uk => "Тривають роздуми; переклад буде після завершення...",
    }
}

pub fn thinking_translation_in_progress(locale: Locale) -> &'static str {
    match locale {
        Locale::En => "Translating thinking content...",
        Locale::Ja => "思考内容を翻訳中...",
        Locale::ZhHans => "正在翻译思考内容...",
        Locale::ZhHant => "正在翻譯思考內容...",
        Locale::PtBr => "Traduzindo o conteúdo de raciocínio...",
        Locale::Es419 => "Traduciendo el contenido de razonamiento...",
        Locale::Vi => "Đang dịch nội dung suy nghĩ...",
        Locale::Ko => "생각 내용을 번역하는 중...",
        Locale::Ca => "S'està traduint el contingut del raonament...",
        Locale::De => "Denkinhalte werden übersetzt...",
        Locale::Fr => "Traduction du contenu de réflexion...",
        Locale::Id => "Menerjemahkan konten pemikiran...",
        Locale::Hi => "विचार सामग्री का अनुवाद हो रहा है...",
        Locale::Ru => "Перевод содержимого рассуждений...",
        Locale::Uk => "Переклад вмісту міркувань...",
    }
}

pub fn thinking_translation_complete(locale: Locale) -> &'static str {
    match locale {
        Locale::En => "Thinking translation complete",
        Locale::Ja => "思考内容の翻訳が完了しました",
        Locale::ZhHans => "思考内容翻译完成",
        Locale::ZhHant => "思考內容翻譯完成",
        Locale::PtBr => "Tradução do raciocínio concluída",
        Locale::Es419 => "Traducción del razonamiento completada",
        Locale::Vi => "Đã dịch xong nội dung suy nghĩ",
        Locale::Ko => "생각 내용 번역 완료",
        Locale::Ca => "Traducció del raonament completada",
        Locale::De => "Übersetzung der Denkinhalte abgeschlossen",
        Locale::Fr => "Traduction de la réflexion terminée",
        Locale::Id => "Terjemahan pemikiran selesai",
        Locale::Hi => "विचार अनुवाद पूरा हुआ",
        Locale::Ru => "Перевод рассуждений завершён",
        Locale::Uk => "Переклад міркувань завершено",
    }
}

pub fn thinking_translation_failed(locale: Locale) -> &'static str {
    match locale {
        Locale::En => "Thinking translation failed",
        Locale::Ja => "思考内容の翻訳に失敗しました",
        Locale::ZhHans => "思考内容翻译失败",
        Locale::ZhHant => "思考內容翻譯失敗",
        Locale::PtBr => "Falha ao traduzir o raciocínio",
        Locale::Es419 => "Falló la traducción del razonamiento",
        Locale::Vi => "Dịch nội dung suy nghĩ thất bại",
        Locale::Ko => "생각 내용 번역 실패",
        Locale::Ca => "Ha fallat la traducció del raonament",
        Locale::De => "Übersetzung der Denkinhalte fehlgeschlagen",
        Locale::Fr => "Échec de la traduction de la réflexion",
        Locale::Id => "Terjemahan pemikiran gagal",
        Locale::Hi => "विचार अनुवाद विफल",
        Locale::Ru => "Не удалось перевести рассуждения",
        Locale::Uk => "Не вдалося перекласти міркування",
    }
}

pub fn hidden_translation_failed(locale: Locale) -> &'static str {
    match locale {
        Locale::En => "Translation failed; original text is hidden.",
        Locale::Ja => "翻訳に失敗しました。原文は非表示です。",
        Locale::ZhHans => "翻译失败，原文已隐藏。",
        Locale::ZhHant => "翻譯失敗，原文已隱藏。",
        Locale::PtBr => "A tradução falhou; o texto original está oculto.",
        Locale::Es419 => "La traducción falló; el texto original está oculto.",
        Locale::Vi => "Dịch thất bại; văn bản gốc đã bị ẩn.",
        Locale::Ko => "번역에 실패했습니다. 원문은 숨겨져 있습니다.",
        Locale::Ca => "La traducció ha fallat; el text original està amagat.",
        Locale::De => "Übersetzung fehlgeschlagen; der Originaltext ist ausgeblendet.",
        Locale::Fr => "La traduction a échoué ; le texte original est masqué.",
        Locale::Id => "Terjemahan gagal; teks asli disembunyikan.",
        Locale::Hi => "अनुवाद विफल; मूल पाठ छिपा हुआ है.",
        Locale::Ru => "Перевод не удался; исходный текст скрыт.",
        Locale::Uk => "Переклад не вдався; оригінальний текст приховано.",
    }
}

pub fn normalize_configured_locale(input: &str) -> Option<&'static str> {
    let normalized = normalize_locale_input(input);
    if matches!(normalized.as_str(), "" | "auto" | "system") {
        return Some("auto");
    }
    parse_locale(&normalized).map(Locale::tag)
}

/// Whether a configured locale selects a shipped pack that intentionally
/// relies on English fallback for missing messages.
#[must_use]
pub fn configured_locale_is_partial_pack(input: &str) -> bool {
    let normalized = normalize_locale_input(input);
    if matches!(normalized.as_str(), "" | "auto" | "system") {
        return false;
    }
    parse_locale(&normalized).is_some_and(|locale| {
        Locale::shipped().contains(&locale)
            && locale.is_partial_pack()
            && !Locale::shipped_complete().contains(&locale)
    })
}

/// Human-facing list of accepted `locale` setting values, derived from the
/// shipped packs so config hints and error messages cannot go stale as new
/// locales land. `separator` is `", "` for prose and `" | "` for hints.
#[must_use]
pub fn configured_locale_values(separator: &str) -> String {
    let mut out = String::from("auto");
    for locale in Locale::shipped() {
        out.push_str(separator);
        out.push_str(locale.tag());
    }
    out
}

pub fn resolve_locale(setting: &str) -> Locale {
    resolve_locale_with_env(setting, |key| std::env::var(key).ok())
}

pub fn resolve_locale_with_env<F>(setting: &str, env: F) -> Locale
where
    F: Fn(&str) -> Option<String>,
{
    let normalized = normalize_locale_input(setting);
    if !matches!(normalized.as_str(), "" | "auto" | "system") {
        return parse_locale(&normalized).unwrap_or(Locale::En);
    }

    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Some(value) = env(key)
            && let Some(locale) = parse_locale(&normalize_locale_input(&value))
        {
            return locale;
        }
    }

    Locale::En
}

#[allow(dead_code)]
pub fn truncate_to_width(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if text.width() <= max_width {
        return text.to_string();
    }

    let ellipsis_width = '…'.width().unwrap_or(1);
    if max_width <= ellipsis_width {
        return "…".to_string();
    }

    let limit = max_width - ellipsis_width;
    let mut out = String::new();
    let mut width = 0usize;
    // Iterate extended grapheme clusters, not chars: a Devanagari conjunct
    // (क + ् + ष), a combined mark (e + ́), or a ZWJ emoji sequence must
    // never be cut apart — a trailing virama or orphaned combining mark
    // renders as visibly broken shaping in the terminal.
    for cluster in text.graphemes(true) {
        let cluster_width = UnicodeWidthStr::width(cluster);
        if width + cluster_width > limit {
            break;
        }
        out.push_str(cluster);
        width += cluster_width;
    }
    out.push('…');
    out
}

fn normalize_locale_input(input: &str) -> String {
    input
        .split('.')
        .next()
        .unwrap_or(input)
        .split('@')
        .next()
        .unwrap_or(input)
        .trim()
        .replace('_', "-")
        .to_lowercase()
}

fn parse_locale(value: &str) -> Option<Locale> {
    if value == "c" || value == "posix" || value.starts_with("en") {
        return Some(Locale::En);
    }
    if value.starts_with("ja") {
        return Some(Locale::Ja);
    }
    if value.starts_with("zh") {
        if value.contains("hant")
            || value.contains("-tw")
            || value.contains("-hk")
            || value.contains("-mo")
        {
            return Some(Locale::ZhHant);
        }
        return Some(Locale::ZhHans);
    }
    if value.starts_with("pt") || value == "br" {
        return Some(Locale::PtBr);
    }
    if value.starts_with("es") {
        return Some(Locale::Es419);
    }
    if value.starts_with("vi") {
        return Some(Locale::Vi);
    }
    if value.starts_with("ko") {
        return Some(Locale::Ko);
    }
    if value.starts_with("ca") {
        return Some(Locale::Ca);
    }
    if value.starts_with("de") {
        return Some(Locale::De);
    }
    if value.starts_with("fr") {
        return Some(Locale::Fr);
    }
    if value.starts_with("id") {
        return Some(Locale::Id);
    }
    if value.starts_with("hi") {
        return Some(Locale::Hi);
    }
    if value.starts_with("ru") {
        return Some(Locale::Ru);
    }
    if value.starts_with("uk") {
        return Some(Locale::Uk);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{
        buffer::Buffer,
        layout::Rect,
        widgets::{Paragraph, Widget, Wrap},
    };

    #[test]
    fn locale_setting_normalizes_supported_tags() {
        assert_eq!(normalize_configured_locale("auto"), Some("auto"));
        assert_eq!(normalize_configured_locale("ja_JP.UTF-8"), Some("ja"));
        assert_eq!(normalize_configured_locale("zh-CN"), Some("zh-Hans"));
        assert_eq!(normalize_configured_locale("zh-TW"), Some("zh-Hant"));
        assert_eq!(normalize_configured_locale("zh_HK.UTF-8"), Some("zh-Hant"));
        assert_eq!(normalize_configured_locale("pt"), Some("pt-BR"));
        assert_eq!(normalize_configured_locale("pt-PT"), Some("pt-BR"));
        assert_eq!(normalize_configured_locale("es"), Some("es-419"));
        assert_eq!(normalize_configured_locale("es-MX"), Some("es-419"));
        assert_eq!(normalize_configured_locale("ca-ES"), Some("ca"));
        assert_eq!(normalize_configured_locale("de_DE.UTF-8"), Some("de"));
        assert_eq!(normalize_configured_locale("fr-FR"), Some("fr"));
        assert_eq!(normalize_configured_locale("id-ID"), Some("id"));
        assert_eq!(normalize_configured_locale("hi_IN.UTF-8"), Some("hi"));
        assert_eq!(normalize_configured_locale("ru-RU"), Some("ru"));
        assert_eq!(normalize_configured_locale("uk_UA.UTF-8"), Some("uk"));
    }

    #[test]
    fn partial_pack_status_tracks_the_shipped_locale_registry() {
        assert!(!configured_locale_is_partial_pack("auto"));
        assert!(!configured_locale_is_partial_pack("system"));
        assert!(!configured_locale_is_partial_pack("zh-Hant"));
        assert!(!configured_locale_is_partial_pack("zh_TW.UTF-8"));
        assert!(!configured_locale_is_partial_pack("vi"));
        assert!(!configured_locale_is_partial_pack("ko"));

        for locale in Locale::shipped() {
            assert_eq!(
                configured_locale_is_partial_pack(locale.tag()),
                locale.is_partial_pack(),
                "{} partial-pack classification drifted",
                locale.tag()
            );
            assert_ne!(
                Locale::shipped_complete().contains(locale),
                locale.is_partial_pack(),
                "{} must be exactly one of complete or partial",
                locale.tag()
            );
        }
    }

    #[test]
    fn locale_resolution_uses_config_then_environment_then_english() {
        assert_eq!(
            resolve_locale_with_env("ja", |_| Some("pt_BR.UTF-8".to_string())),
            Locale::Ja
        );
        assert_eq!(
            resolve_locale_with_env("auto", |key| {
                (key == "LANG").then(|| "zh_CN.UTF-8".to_string())
            }),
            Locale::ZhHans
        );
        assert_eq!(
            resolve_locale_with_env("auto", |key| {
                (key == "LANG").then(|| "zh_TW.UTF-8".to_string())
            }),
            Locale::ZhHant
        );
        assert_eq!(resolve_locale_with_env("auto", |_| None), Locale::En);
    }

    pub fn missing_message_ids(locale: Locale) -> Vec<MessageId> {
        ALL_MESSAGE_IDS
            .iter()
            .copied()
            .filter(|id| tr(locale, *id).eq(&format!("{id:?}")))
            .collect()
    }

    fn locale_json_source(locale: Locale) -> &'static str {
        match locale {
            Locale::En => include_str!("../locales/en.json"),
            Locale::Ja => include_str!("../locales/ja.json"),
            Locale::ZhHans => include_str!("../locales/zh-Hans.json"),
            Locale::ZhHant => include_str!("../locales/zh-Hant.json"),
            Locale::PtBr => include_str!("../locales/pt-BR.json"),
            Locale::Es419 => include_str!("../locales/es-419.json"),
            Locale::Vi => include_str!("../locales/vi.json"),
            Locale::Ko => include_str!("../locales/ko.json"),
            Locale::Ca => include_str!("../locales/ca.json"),
            Locale::De => include_str!("../locales/de.json"),
            Locale::Fr => include_str!("../locales/fr.json"),
            Locale::Id => include_str!("../locales/id.json"),
            Locale::Hi => include_str!("../locales/hi.json"),
            Locale::Ru => include_str!("../locales/ru.json"),
            Locale::Uk => include_str!("../locales/uk.json"),
        }
    }

    #[test]
    fn shipped_complete_packs_have_no_missing_core_messages() {
        for locale in Locale::shipped_complete() {
            assert!(
                missing_message_ids(*locale).is_empty(),
                "{} is missing messages",
                locale.tag()
            );
        }
    }

    #[test]
    fn work_stop_confirmation_is_explicitly_localized() {
        for locale in Locale::shipped_complete() {
            if *locale == Locale::En {
                continue;
            }
            assert_ne!(tr(*locale, MessageId::SidebarStopControl), "stop");
            assert_ne!(
                tr(*locale, MessageId::WorkSurfaceStopConfirmHint),
                "confirm stop · Esc cancels"
            );
        }
    }

    #[test]
    fn coordination_work_chrome_is_explicitly_localized() {
        for locale in Locale::shipped_complete() {
            if *locale == Locale::En {
                continue;
            }
            assert_ne!(
                tr(*locale, MessageId::CoordinationWorkTitle),
                tr(Locale::En, MessageId::CoordinationWorkTitle),
                "{} fell back to the English Coordination Work title",
                locale.tag()
            );
            assert_ne!(
                tr(*locale, MessageId::CoordinationMetricsNoAuthoritativeSource),
                tr(
                    Locale::En,
                    MessageId::CoordinationMetricsNoAuthoritativeSource
                ),
                "{} fell back to the English coordination metrics note",
                locale.tag()
            );
        }
    }

    #[test]
    fn context_inspector_and_command_hints_are_explicitly_localized() {
        let ids = [
            MessageId::CtxInspRowCompaction,
            MessageId::CtxInspRowAnchors,
            MessageId::CtxInspCompactionNever,
            MessageId::CtxInspCompactionDetail,
            MessageId::CtxInspCompactionRestored,
            MessageId::CtxInspCompactionPathSummary,
            MessageId::CtxInspCompactionPathPrune,
            MessageId::CtxInspCompactionAssistantKept,
            MessageId::CtxInspAnchorsNone,
            MessageId::CtxInspAnchorsPresent,
            MessageId::KbReasoningDetail,
            MessageId::CmdTurnInspectDescription,
            MessageId::CmdAdvisorDescription,
            MessageId::ModelChangedSessionNote,
        ];
        for locale in Locale::shipped_complete() {
            if *locale == Locale::En {
                continue;
            }
            for id in ids {
                assert_ne!(
                    tr(*locale, id),
                    tr(Locale::En, id),
                    "{} ships the English text for {id:?}",
                    locale.tag()
                );
            }
        }
    }

    fn raw_locale_messages(locale: Locale) -> serde_json::Map<String, serde_json::Value> {
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(locale_json_source(
            locale,
        ))
        .unwrap_or_else(|err| panic!("{} locale json should parse: {err}", locale.tag()))
    }

    fn raw_locale_keys(locale: Locale) -> std::collections::BTreeSet<String> {
        raw_locale_messages(locale).keys().cloned().collect()
    }

    fn message_placeholders(value: &str) -> std::collections::BTreeSet<String> {
        value
            .split('{')
            .skip(1)
            .filter_map(|suffix| suffix.split_once('}').map(|(name, _)| name.to_string()))
            .collect()
    }

    #[test]
    fn plugin_owner_report_keeps_translated_state_and_placeholder_parity() {
        let english = raw_locale_messages(Locale::En);
        let keys: Vec<_> = english
            .keys()
            .filter(|key| key.starts_with("CmdPluginOwner"))
            .collect();
        assert_eq!(keys.len(), 7);
        for locale in Locale::shipped_complete() {
            let pack = raw_locale_messages(*locale);
            for key in &keys {
                let original = english[*key].as_str().unwrap();
                let translated = pack[*key].as_str().unwrap();
                assert_eq!(
                    message_placeholders(translated),
                    message_placeholders(original),
                    "{} changed placeholders for {key}",
                    locale.tag()
                );
                if *locale != Locale::En {
                    assert_ne!(
                        translated,
                        original,
                        "{} left {key} in English",
                        locale.tag()
                    );
                }
            }
        }
    }

    /// #5906: the parked-agent vocabulary is new copy on the busiest rows in
    /// the product, so it gets the same hard parity gate coordination copy
    /// has — and the recovery line must keep the tool tokens it names, or it
    /// tells an operator to type a verb that does not exist.
    #[test]
    fn parked_agent_copy_is_translated_and_keeps_its_tool_verbs() {
        for locale in Locale::shipped_complete() {
            let word = tr(*locale, MessageId::AgentStatusParked);
            assert!(
                !word.trim().is_empty(),
                "{} has no parked status word",
                locale.tag()
            );
            let recovery = tr(*locale, MessageId::AgentStatusParkedRecovery);
            assert!(
                recovery.contains("resume_from"),
                "{} lost the resume verb the agent tool exposes: {recovery}",
                locale.tag()
            );
            assert!(
                recovery.contains("cancel"),
                "{} lost the dismiss verb: {recovery}",
                locale.tag()
            );
            if *locale == Locale::En {
                continue;
            }
            assert_ne!(
                word,
                tr(Locale::En, MessageId::AgentStatusParked),
                "{} fell back to the English parked status word",
                locale.tag()
            );
            assert_ne!(
                recovery,
                tr(Locale::En, MessageId::AgentStatusParkedRecovery),
                "{} fell back to the English parked recovery line",
                locale.tag()
            );
        }
    }

    #[test]
    fn background_review_complete_packs_translate_copy_and_preserve_placeholders() {
        let english = raw_locale_messages(Locale::En);
        let keys = english
            .keys()
            .filter(|key| {
                key.starts_with("Background") || key.as_str() == "NotificationBackgroundStopped"
            })
            .collect::<Vec<_>>();
        assert_eq!(keys.len(), 13);
        for &locale in Locale::shipped_complete() {
            let pack = raw_locale_messages(locale);
            for key in &keys {
                let source = english[*key].as_str().unwrap();
                let translated = pack
                    .get(*key)
                    .and_then(serde_json::Value::as_str)
                    .expect("translated background key");
                assert_eq!(
                    message_placeholders(translated),
                    message_placeholders(source),
                    "{} {key}",
                    locale.tag()
                );
                if locale != Locale::En {
                    assert_ne!(translated, source, "{} {key}", locale.tag());
                }
            }
        }
    }

    #[test]
    fn coordination_complete_packs_have_raw_key_and_placeholder_parity() {
        let english = raw_locale_messages(Locale::En);
        let coordination_keys = english
            .keys()
            .filter(|key| key.starts_with("Coordination"))
            .collect::<Vec<_>>();
        assert_eq!(coordination_keys.len(), 39);

        for locale in Locale::shipped_complete() {
            let pack = raw_locale_messages(*locale);
            for key in &coordination_keys {
                let english_value = english
                    .get(*key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_else(|| panic!("English {key} must be a string"));
                let translated = pack
                    .get(*key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_else(|| panic!("{} is missing raw key {key}", locale.tag()));
                assert_eq!(
                    message_placeholders(translated),
                    message_placeholders(english_value),
                    "{} changed placeholders for {key}",
                    locale.tag()
                );
            }
        }
    }

    #[test]
    fn automation_complete_packs_have_raw_key_and_placeholder_parity() {
        let english = raw_locale_messages(Locale::En);
        let automation_keys = english
            .keys()
            .filter(|key| key.starts_with("Automation"))
            .collect::<Vec<_>>();
        // New room/editor strings are part of the same complete-pack contract.
        assert!(!automation_keys.is_empty());

        for locale in Locale::shipped_complete() {
            let pack = raw_locale_messages(*locale);
            for key in &automation_keys {
                let english_value = english
                    .get(*key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_else(|| panic!("English {key} must be a string"));
                let translated = pack
                    .get(*key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_else(|| panic!("{} is missing raw key {key}", locale.tag()));
                assert_eq!(
                    message_placeholders(translated),
                    message_placeholders(english_value),
                    "{} changed placeholders for {key}",
                    locale.tag()
                );
            }
        }
    }

    /// The `/cost` and `/tokens` honesty block is assembled by `{placeholder}`
    /// substitution, so a translation that drops or renames one silently ships a
    /// line with a literal `{priced}` in it — or worse, omits the count that
    /// makes the sentence true. Cost copy is exactly where a mistranslation
    /// becomes a false claim about money, so it gets the same hard parity gate
    /// the coordination pack has (#4318).
    #[test]
    fn cost_copy_has_raw_key_and_placeholder_parity_across_complete_packs() {
        let english = raw_locale_messages(Locale::En);
        let cost_keys = english
            .keys()
            .filter(|key| key.starts_with("CmdCost") || key.starts_with("CmdTokensCache"))
            .cloned()
            .collect::<Vec<_>>();
        // Guard against the filter silently matching nothing after a rename.
        assert!(
            cost_keys.len() >= 12,
            "expected the full CmdCost*/CmdTokensCache* set, found {cost_keys:?}"
        );
        // The keys this pass added must be in the set the gate covers.
        for required in [
            "CmdCostEstimateOnly",
            "CmdCostCoverage",
            "CmdCostCoverageUnknownLegacy",
            "CmdCostUnpricedTurns",
            "CmdCostUnpricedClasses",
            "CmdCostPricingProvenance",
            "CmdCostLivePricingDowngraded",
            "CmdCostLivePricingUnavailable",
            "CmdCostRoutesHeader",
            "CmdTokensCacheWriteTotal",
        ] {
            assert!(
                cost_keys.iter().any(|key| key == required),
                "{required} is missing from en.json"
            );
        }

        for locale in Locale::shipped_complete() {
            let pack = raw_locale_messages(*locale);
            for key in &cost_keys {
                let english_value = english
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_else(|| panic!("English {key} must be a string"));
                let translated = pack
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_else(|| panic!("{} is missing raw key {key}", locale.tag()));
                assert_eq!(
                    message_placeholders(translated),
                    message_placeholders(english_value),
                    "{} changed placeholders for {key}",
                    locale.tag()
                );
            }
        }
    }

    /// Key parity proves a pack *has* the subtotal and audited-route lines; it
    /// does not prove anyone translated them. A pack that copies the English
    /// string passes every structural gate and still ships English text to a
    /// Japanese user — and these two lines are the ones that say a money figure
    /// is incomplete and name the routes it was built from, which is exactly
    /// the copy a reader must be able to understand (#4318).
    #[test]
    fn every_complete_pack_localizes_the_subtotal_and_audited_route_copy() {
        for locale in Locale::shipped_complete()
            .iter()
            .filter(|locale| **locale != Locale::En)
        {
            for id in [
                MessageId::CmdCostReportSubtotal,
                MessageId::CmdCostReportUnknown,
                MessageId::CmdCostRoutesHeader,
                MessageId::CmdCostUnknownValue,
                MessageId::CmdCostCoverageUnknownLegacy,
            ] {
                let localized = tr(*locale, id);
                let english = tr(Locale::En, id);
                assert!(
                    !localized.trim().is_empty(),
                    "{} has empty copy for {id:?}",
                    locale.tag()
                );
                assert_ne!(
                    localized,
                    english,
                    "{} still ships the English string for {id:?}",
                    locale.tag()
                );
            }
            // The subtotal headline must still carry its amount, and must not
            // reuse the complete-total wording — those two states are the whole
            // point of having separate keys.
            let subtotal = tr(*locale, MessageId::CmdCostReportSubtotal);
            assert!(
                subtotal.contains("{cost}"),
                "{} subtotal headline lost its amount",
                locale.tag()
            );
            assert_ne!(
                subtotal,
                tr(*locale, MessageId::CmdCostReport),
                "{} cannot distinguish a subtotal from a complete total",
                locale.tag()
            );
            // The unknown headline names no amount at all.
            let unknown = tr(*locale, MessageId::CmdCostReportUnknown);
            assert!(
                !unknown.contains("{cost}"),
                "{} unknown headline must not interpolate an amount",
                locale.tag()
            );
        }
    }

    /// Both money surfaces must say "estimate". `/tokens` quotes the same total
    /// as `/cost`, so it cannot present it as settled while `/cost` hedges.
    #[test]
    fn every_complete_pack_marks_the_cost_total_as_an_estimate() {
        for locale in Locale::shipped_complete() {
            let disclaimer = tr(*locale, MessageId::CmdCostEstimateOnly);
            assert!(
                !disclaimer.trim().is_empty(),
                "{} has no cost estimate disclaimer",
                locale.tag()
            );
            let coverage = tr(*locale, MessageId::CmdCostCoverage);
            assert!(
                coverage.contains("{priced}") && coverage.contains("{turns}"),
                "{} coverage line lost its counts",
                locale.tag()
            );
        }
    }

    /// `missing_message_ids` is blind to keys that exist in en but not in a
    /// "complete" pack — the English fallback returns the English string, so
    /// nothing looks missing. Keep the enum, en.json, and ALL_MESSAGE_IDS in
    /// exact sync so every other parity gate actually sees every message.
    #[test]
    fn message_id_list_english_pack_stay_in_exact_sync() {
        let en = raw_locale_keys(Locale::En);
        let ids: std::collections::BTreeSet<String> =
            ALL_MESSAGE_IDS.iter().map(|id| format!("{id:?}")).collect();
        assert_eq!(
            ids.len(),
            ALL_MESSAGE_IDS.len(),
            "ALL_MESSAGE_IDS contains duplicates"
        );
        let unlisted: Vec<_> = en.difference(&ids).collect();
        assert!(
            unlisted.is_empty(),
            "en.json keys absent from ALL_MESSAGE_IDS — every parity test is blind to them: {unlisted:?}"
        );
        let untranslatable: Vec<_> = ids.difference(&en).collect();
        assert!(
            untranslatable.is_empty(),
            "ALL_MESSAGE_IDS entries without an en.json string: {untranslatable:?}"
        );
    }

    /// Raw key-set parity for every pack that claims completeness, in both
    /// directions. This is the test that fails when a new en key ships
    /// without translations instead of silently falling back to English.
    #[test]
    fn shipped_complete_packs_have_raw_key_parity_with_english() {
        let en = raw_locale_keys(Locale::En);
        for locale in Locale::shipped_complete() {
            if *locale == Locale::En {
                continue;
            }
            let pack = raw_locale_keys(*locale);
            let missing: Vec<_> = en.difference(&pack).collect();
            assert!(
                missing.is_empty(),
                "{} claims completeness but lacks {} key(s); the English fallback hides these at runtime: {missing:?}",
                locale.tag(),
                missing.len()
            );
            let extra: Vec<_> = pack.difference(&en).collect();
            assert!(
                extra.is_empty(),
                "{} defines key(s) en.json lacks: {extra:?}",
                locale.tag()
            );
        }
    }

    /// #6715 review: the sign-in account summary is filled by `{provider}` and
    /// `{account}` substitution, so a pack that drops a placeholder or ships the
    /// English sentence would silently name no account or stay untranslated.
    #[test]
    fn auth_sign_in_copy_is_translated_and_keeps_its_placeholders() {
        let english = raw_locale_messages(Locale::En);
        let auth_keys = [
            "AuthSignedInAs",
            "AuthSignedInWithoutEmail",
            "AuthReplacedPreviousSignInAs",
            "AuthReplacedPreviousSignIn",
            "AuthSameAccountAsBefore",
            "AuthEnvTokenOutranksSignIn",
        ];

        for locale in Locale::shipped_complete() {
            if *locale == Locale::En {
                continue;
            }
            let pack = raw_locale_messages(*locale);
            for key in auth_keys {
                let english_value = english
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_else(|| panic!("English {key} must be a string"));
                let translated = pack
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_else(|| panic!("{} is missing raw key {key}", locale.tag()));
                assert_eq!(
                    message_placeholders(translated),
                    message_placeholders(english_value),
                    "{} changed placeholders for {key}",
                    locale.tag()
                );
                assert_ne!(
                    translated,
                    english_value,
                    "{} ships English for {key}",
                    locale.tag()
                );
            }
        }
    }

    #[test]
    fn current_session_pod_worker_copy_has_complete_locale_and_placeholder_parity() {
        let current_session_ids = [
            MessageId::SubagentsNoCurrentSessionFleetWorkers,
            MessageId::SubagentsCurrentSessionFleetWorkersTitle,
            MessageId::SubagentsCurrentSessionFleetWorkerRoles,
            MessageId::SubagentsCurrentSessionFleetWorkersStatus,
        ];
        let modal_ids = [
            MessageId::SubagentsEmptyGuidance,
            MessageId::SubagentsStatusRunning,
            MessageId::SubagentsStatusCompleted,
            MessageId::SubagentsStatusInterrupted,
            MessageId::SubagentsStatusFailed,
            MessageId::SubagentsStatusCancelled,
            MessageId::SubagentsRowStatusInterrupted,
            MessageId::SubagentsRowStatusCancelled,
            MessageId::SubagentsRowStatusBudgetExhausted,
            MessageId::SubagentsSummaryItem,
            MessageId::SubagentsGroupHeading,
            MessageId::SubagentsHeaderRoster,
            MessageId::SubagentsHeaderColumns,
            MessageId::SubagentsActionRefresh,
            MessageId::SubagentsActionRosterSetup,
            MessageId::SubagentsLabelReason,
            MessageId::SubagentsLabelRole,
            MessageId::SubagentsLabelPosture,
            MessageId::SubagentsLabelGit,
            MessageId::SubagentsLabelObjective,
            MessageId::SubagentsLabelResult,
            MessageId::SubagentsPostureDetails,
            MessageId::SubagentsValueOn,
            MessageId::SubagentsValueOff,
            MessageId::SubagentsShellNone,
            MessageId::SubagentsShellReadOnly,
            MessageId::SubagentsShellFull,
            MessageId::SubagentsBranch,
            MessageId::SubagentsBranchWithWorkspace,
            MessageId::SubagentsRoleWorker,
            MessageId::SubagentsRoleScout,
            MessageId::SubagentsRolePlanner,
            MessageId::SubagentsRoleBuilder,
            MessageId::SubagentsRoleVerifier,
            MessageId::SubagentsRoleReviewer,
            MessageId::SubagentsRoleConsultant,
            MessageId::SubagentsRoleCustom,
        ];
        let english = raw_locale_messages(Locale::En);

        for locale in Locale::shipped_complete() {
            let pack = raw_locale_messages(*locale);
            for id in current_session_ids.iter().chain(modal_ids.iter()) {
                let id = *id;
                let key = format!("{id:?}");
                let english_value = english
                    .get(&key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_else(|| panic!("English pack is missing {key}"));
                let translated = pack
                    .get(&key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_else(|| panic!("{} is missing {key}", locale.tag()));

                assert_eq!(
                    message_placeholders(translated),
                    message_placeholders(english_value),
                    "{} changed placeholders for {key}",
                    locale.tag()
                );
                if *locale != Locale::En && current_session_ids.contains(&id) {
                    assert_ne!(
                        translated,
                        english_value,
                        "{} must translate {key} instead of copying English",
                        locale.tag()
                    );
                }
            }
        }
    }

    #[test]
    fn home_subagents_keeps_the_current_session_boundary_in_every_complete_pack() {
        let expected = [
            (Locale::Ca, "Treballadors de flota de la sessió actual:"),
            (Locale::De, "Flotten-Worker der aktuellen Sitzung:"),
            (Locale::En, "Agents this session:"),
            (Locale::Es419, "Workers de flota de la sesión actual:"),
            (Locale::Fr, "Workers de la flotte de la session actuelle :"),
            (Locale::Hi, "वर्तमान सत्र के बेड़ा वर्कर:"),
            (Locale::Id, "Worker armada sesi saat ini:"),
            (Locale::Ja, "現在のセッションの艦隊ワーカー："),
            (Locale::Ko, "현재 세션의 플릿 워커:"),
            (Locale::PtBr, "Workers da frota da sessão atual:"),
            (Locale::Ru, "Воркеры флота текущего сеанса:"),
            (Locale::Uk, "Воркери флоту поточного сеансу:"),
            (Locale::Vi, "Worker hạm đội của phiên hiện tại:"),
            (Locale::ZhHans, "本会话的智能体："),
            (Locale::ZhHant, "本工作階段的代理："),
        ];
        assert_eq!(expected.len(), Locale::shipped_complete().len());

        for (locale, value) in expected {
            assert_eq!(
                tr(locale, MessageId::HomeSubagents),
                value,
                "{}",
                locale.tag()
            );
        }
    }

    #[test]
    fn es_419_setup_review_hint_records_only_the_setup_snapshot() {
        assert_eq!(
            tr(Locale::Es419, MessageId::SetupOperateReviewHint),
            "Enter registra esta instantánea de configuración."
        );
    }

    #[test]
    fn status_report_copy_has_placeholder_parity_across_complete_packs() {
        let english = raw_locale_messages(Locale::En);
        let status_ids = ALL_MESSAGE_IDS
            .iter()
            .filter(|id| format!("{id:?}").starts_with("Status"));

        for id in status_ids {
            let key = format!("{id:?}");
            let english_value = english
                .get(&key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_else(|| panic!("English {key} must be a string"));
            for locale in Locale::shipped_complete() {
                let pack = raw_locale_messages(*locale);
                let translated = pack
                    .get(&key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_else(|| panic!("{} is missing raw key {key}", locale.tag()));
                assert_eq!(
                    message_placeholders(translated),
                    message_placeholders(english_value),
                    "{} changed placeholders for {key}",
                    locale.tag()
                );
            }
        }
    }

    #[test]
    fn status_report_copy_preserves_technical_identities_across_complete_packs() {
        let required: &[(MessageId, &[&str])] = &[
            (MessageId::StatusLabelMcp, &["MCP"]),
            (MessageId::StatusContextSourceKimiSafeFloor, &["Kimi Code"]),
            (
                MessageId::StatusWindowOverrideProvider,
                &["[providers.{table}]", "context_window", "config.toml"],
            ),
            (
                MessageId::StatusWindowOverrideActiveProvider,
                &["context_window", "config.toml"],
            ),
            (
                MessageId::StatusSafetyWorkspaceWriteUnenforcedNetworkOn,
                &["workspace-write"],
            ),
            (
                MessageId::StatusSafetyWorkspaceWriteUnenforcedNetworkOff,
                &["workspace-write"],
            ),
            (
                MessageId::StatusSafetyWorkspaceWriteNetworkOn,
                &["workspace-write"],
            ),
            (
                MessageId::StatusSafetyWorkspaceWriteNetworkOff,
                &["workspace-write"],
            ),
            (MessageId::StatusPointers, &["/tokens", "/statusline"]),
        ];

        for locale in Locale::shipped_complete() {
            for (id, literals) in required {
                let translated = tr(*locale, *id);
                for literal in *literals {
                    assert!(
                        translated.contains(literal),
                        "{} changed protected literal {literal:?} in {id:?}: {translated}",
                        locale.tag()
                    );
                }
            }
        }
    }

    #[test]
    fn config_command_prose_is_translated_in_complete_locales() {
        let ids = [
            MessageId::ConfigCommandSource,
            MessageId::ConfigCommandInvalidValue,
            MessageId::ConfigSearchUpdated,
            MessageId::ConfigPromptSuggestionUpdated,
            MessageId::ConfigNotificationsSetHint,
            MessageId::ConfigNotificationUpdated,
            MessageId::ConfigNotificationsWholeNumber,
            MessageId::ConfigAuditSearchProvider,
            MessageId::ConfigAuditPromptSuggestion,
            MessageId::ConfigAuditNotifications,
            MessageId::ConfigHelpDiscoverable,
        ];
        for locale in Locale::shipped_complete() {
            for id in ids {
                let localized = tr(*locale, id);
                assert!(!localized.trim().is_empty(), "{} {id:?}", locale.tag());
                if *locale != Locale::En {
                    assert_ne!(localized, tr(Locale::En, id), "{} {id:?}", locale.tag());
                }
            }
        }

        assert!(tr(Locale::En, MessageId::ConfigCommandSource).contains("{source}"));
        assert!(tr(Locale::En, MessageId::ConfigCommandInvalidValue).contains("{choices}"));
        assert!(tr(Locale::En, MessageId::ConfigNotificationUpdated).contains("{scope}"));
    }

    #[test]
    fn remote_env_strings_are_explicitly_localized_in_every_complete_pack() {
        let ids = [
            MessageId::CmdRemoteEnvDescription,
            MessageId::CmdRemoteEnvOverview,
            MessageId::CmdRemoteEnvOpening,
            MessageId::CmdRemoteEnvUnavailable,
            MessageId::CmdRemoteEnvSourceCustodyPolicy,
            MessageId::CmdRemoteEnvBrowserLabel,
        ];

        for locale in Locale::shipped_complete() {
            let messages = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
                locale_json_source(*locale),
            )
            .unwrap_or_else(|err| panic!("{} locale JSON should parse: {err}", locale.tag()));
            for id in ids {
                let key = format!("{id:?}");
                let value = messages
                    .get(&key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_else(|| panic!("{} must explicitly define {key}", locale.tag()));
                assert!(
                    !value.trim().is_empty(),
                    "{} {key} must not be empty",
                    locale.tag()
                );
            }
        }
    }

    #[test]
    fn todo_write_tip_is_localized_and_keeps_the_command_placeholder() {
        let english = tr(Locale::En, MessageId::BehavioralTipTodoWrite);
        assert!(english.contains("{command}"));

        for locale in Locale::shipped_complete() {
            let tip = tr(*locale, MessageId::BehavioralTipTodoWrite);
            assert!(
                tip.contains("{command}"),
                "{} todo_write tip must compose the command in code",
                locale.tag()
            );
            if *locale != Locale::En {
                assert_ne!(
                    tip,
                    english,
                    "{} todo_write tip must be translated instead of copying English",
                    locale.tag()
                );
            }
        }
    }

    #[test]
    fn zh_hant_has_reached_en_parity_and_is_complete() {
        assert!(
            !Locale::ZhHant.is_partial_pack(),
            "zh-Hant is now a complete pack and must not be marked partial"
        );
        assert!(
            Locale::shipped_complete().contains(&Locale::ZhHant),
            "zh-Hant must be included in shipped_complete now that it has full en.json parity"
        );
        let en_keys = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
            locale_json_source(Locale::En),
        )
        .expect("en locale json");
        let zh_hant_keys = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
            locale_json_source(Locale::ZhHant),
        )
        .expect("zh-Hant locale json");
        assert_eq!(
            zh_hant_keys.len(),
            en_keys.len(),
            "zh-Hant must have the same number of keys as en.json"
        );
    }

    #[test]
    fn shipped_setup_strings_are_explicitly_localized() {
        let setup_keys = ALL_MESSAGE_IDS
            .iter()
            .map(|id| format!("{id:?}"))
            .filter(|id| id.starts_with("Setup"))
            .collect::<Vec<_>>();

        for locale in Locale::shipped_complete() {
            let messages = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
                locale_json_source(*locale),
            )
            .unwrap_or_else(|err| panic!("{} locale json should parse: {err}", locale.tag()));
            for key in &setup_keys {
                assert!(
                    messages.contains_key(key),
                    "{} should define {key} explicitly",
                    locale.tag()
                );
            }
        }
    }

    #[test]
    fn zh_hans_constitution_copy_uses_charter_term() {
        let messages = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
            locale_json_source(Locale::ZhHans),
        )
        .expect("zh-Hans locale json");

        for (key, value) in &messages {
            let Some(value) = value.as_str() else {
                continue;
            };
            for literal_metaphor in ["宪法", "教义", "自由原则", "仓库法则"] {
                assert!(
                    !value.contains(literal_metaphor),
                    "zh-Hans {key} should use functional terminology instead of {literal_metaphor}: {value}"
                );
            }
        }

        let setup_intro = tr(Locale::ZhHans, MessageId::SetupStepConstitutionWhy);
        assert!(setup_intro.contains("Codewhale"));
        assert!(setup_intro.contains("宪章"));
        assert!(!setup_intro.contains("代码"));
        // The romanized-brand guard lives on `setup_intro` above: the welcome
        // lead names commands, not the product, so asserting "Codewhale" here
        // would only force a brand into copy that does not need one (#5442).
        let welcome = tr(Locale::ZhHans, MessageId::OnboardWelcomeLead);
        assert!(!welcome.contains("代码"));
        assert!(
            tr(
                Locale::ZhHans,
                MessageId::SetupConstitutionFileLoadedUnselected
            )
            .contains("constitution.json")
        );
    }

    #[test]
    fn home_quick_rows_name_flagship_capabilities_in_every_complete_pack() {
        // #5442: /home must name the shipped surfaces a new user never finds
        // from governance copy alone. First-run onboarding no longer carries a
        // command tour — contextual help and /setup own that job — so the
        // flagship-command guard lives on the /home surface that still shows it.
        for locale in Locale::shipped_complete() {
            for id in [
                MessageId::HomeQuickWorkspace,
                MessageId::HomeQuickRestore,
                MessageId::HomeQuickTokens,
            ] {
                let text = tr(*locale, id);
                assert!(!text.trim().is_empty(), "{} {id:?} is empty", locale.tag());
            }
            assert!(
                tr(*locale, MessageId::HomeQuickWorkspace).contains("/workspace"),
                "{} /home lost /workspace",
                locale.tag()
            );
            assert!(
                tr(*locale, MessageId::HomeQuickRestore).contains("/restore"),
                "{} /home lost /restore",
                locale.tag()
            );
            assert!(
                tr(*locale, MessageId::HomeQuickTokens).contains("/tokens"),
                "{} /home lost /tokens",
                locale.tag()
            );
        }
    }

    #[test]
    fn home_quick_action_rows_share_one_command_column_in_every_pack() {
        // The quick-action block is a fixed-width list. The command name and
        // its padding are composed in English and must survive translation
        // byte-for-byte, or the column goes ragged in that locale alone.
        const ROWS: &[MessageId] = &[
            MessageId::HomeQuickWorkspace,
            MessageId::HomeQuickRestore,
            MessageId::HomeQuickTokens,
            MessageId::HomeQuickLinks,
            MessageId::HomeQuickSkills,
            MessageId::HomeQuickConfig,
            MessageId::HomeQuickSettings,
            MessageId::HomeQuickModel,
            MessageId::HomeQuickSubagents,
            MessageId::HomeQuickTaskList,
            MessageId::HomeQuickHelp,
        ];
        for id in ROWS {
            let english = tr(Locale::En, *id);
            let dash = english.find(" - ").expect("quick-action row separator");
            let prefix = &english[..dash + " - ".len()];
            for locale in Locale::shipped_complete() {
                let row = tr(*locale, *id);
                assert!(
                    row.starts_with(prefix),
                    "{} {id:?} moved the command column: expected prefix {prefix:?}, got {row:?}",
                    locale.tag()
                );
            }
        }
    }

    #[test]
    fn restore_copy_never_promises_to_rewind_the_conversation() {
        // #5442: `/restore` rolls *workspace files* back to a snapshot. `/undo`
        // is what drops a conversation turn. Copy that says "rewind a turn"
        // sends new users to the wrong command.
        for locale in Locale::shipped_complete() {
            let text = tr(*locale, MessageId::HomeQuickRestore);
            assert!(
                text.contains("/restore"),
                "{} HomeQuickRestore stopped naming /restore: {text}",
                locale.tag()
            );
        }
        let english = tr(Locale::En, MessageId::HomeQuickRestore);
        assert!(
            !english.contains("rewind a turn") && !english.contains("rewind turn"),
            "HomeQuickRestore describes /restore as rewinding a turn: {english}"
        );
    }

    #[test]
    fn route_and_provider_picker_strings_are_translated_in_complete_locales() {
        // High-visibility model/provider empty states and footers must not
        // leak English through the fallback chain in complete packs.
        let ids = [
            MessageId::PickerActionMove,
            MessageId::PickerActionSwitch,
            MessageId::PickerActionApply,
            MessageId::PickerActionAssignRoute,
            MessageId::FleetRoutePickUnavailable,
            MessageId::FleetRouteSaved,
            MessageId::FleetRouteInherited,
            MessageId::FleetRouteNotInCatalog,
            MessageId::StatusFleetDrifted,
            MessageId::StatusModelNotInRoster,
            MessageId::PickerActionSetStartupDefault,
            MessageId::PickerActionPin,
            MessageId::PickerActionFleet,
            MessageId::PickerActionCancel,
            MessageId::PickerActionClear,
            MessageId::PickerActionClearSearch,
            MessageId::PickerActionBrowseAll,
            MessageId::PickerActionCustom,
            MessageId::PickerActionJump,
            MessageId::PickerActionEditKey,
            MessageId::PickerActionModels,
            MessageId::PickerActionConfigured,
            MessageId::RouteNoModels,
            MessageId::RouteNoModelMatch,
            MessageId::ProviderNoMatchesTitle,
            MessageId::ProviderNoMatchesHint,
            MessageId::ProviderNoConfiguredTitle,
            MessageId::ProviderNoConfiguredHint,
            MessageId::ProviderNoCatalogModels,
            MessageId::ProviderCustomFormBaseUrl,
            MessageId::ProviderCustomFormModel,
            MessageId::ConfigHintProviderUrl,
            MessageId::SessionsOpenedHistory,
            MessageId::SessionsTimeJustNow,
        ];
        for locale in Locale::shipped_complete() {
            if *locale == Locale::En {
                continue;
            }
            for id in ids {
                let localized = tr(*locale, id);
                assert!(!localized.is_empty(), "{} empty for {id:?}", locale.tag());
                // Catalan "models" is the correct translation of the English
                // picker action — the words coincide. Every other id must
                // differ from English, or the pack is leaking the fallback.
                if matches!((*locale, id), (Locale::Ca, MessageId::PickerActionModels)) {
                    continue;
                }
                assert_ne!(
                    localized,
                    tr(Locale::En, id),
                    "{} should translate {id:?}",
                    locale.tag()
                );
            }
        }
    }

    #[test]
    fn launch_copy_is_translated_in_complete_locales() {
        let ids = [
            MessageId::ComposerPlaceholder,
            MessageId::ComposerPlaceholderFollowUp,
            MessageId::ComposerPlaceholderSendNow,
            MessageId::ComposerHintQueue,
            MessageId::EmptyStatePrompt,
        ];
        for locale in Locale::shipped_complete() {
            if *locale == Locale::En {
                continue;
            }
            for id in ids {
                let localized = tr(*locale, id);
                assert!(!localized.is_empty(), "{} empty for {id:?}", locale.tag());
                assert_ne!(
                    localized,
                    tr(Locale::En, id),
                    "{} should translate {id:?}",
                    locale.tag()
                );
            }
        }
    }

    #[test]
    fn launch_choice_and_readiness_prose_is_translated_in_complete_locales() {
        let ids = [
            MessageId::LaunchStartTitle,
            MessageId::LaunchWorkDescription,
            MessageId::LaunchChatDescription,
            MessageId::LaunchWorkspaceFolderReady,
            MessageId::LaunchProviderSetupNeeded,
            MessageId::LaunchNewSession,
            MessageId::LaunchRecentHeading,
            MessageId::LaunchSeeAllSessions,
            MessageId::LaunchNoRecentSessions,
            MessageId::LaunchResumeFailed,
            MessageId::LaunchNoticeClaude,
        ];
        for locale in Locale::shipped_complete() {
            if *locale == Locale::En {
                continue;
            }
            for id in ids {
                let localized = tr(*locale, id);
                assert!(!localized.is_empty(), "{} empty for {id:?}", locale.tag());
                assert_ne!(
                    localized,
                    tr(Locale::En, id),
                    "{} should translate {id:?}",
                    locale.tag()
                );
            }
        }
    }

    #[test]
    fn kimi_import_and_new_mcp_recommendations_have_complete_locale_parity() {
        let ids = [
            MessageId::McpRecommendedUnknownId,
            MessageId::McpRecommendationsHeading,
            MessageId::McpRecommendationsSafety,
            MessageId::McpRecommendationGithub,
            MessageId::McpRecommendationChrome,
            MessageId::McpRecommendationPlaywright,
            MessageId::McpRecommendationContainerUse,
            MessageId::PluginKimiUsage,
            MessageId::PluginKimiManagedRootHeading,
            MessageId::PluginKimiNoneFound,
            MessageId::PluginKimiLicenseUnspecified,
            MessageId::PluginKimiApplicable,
            MessageId::PluginKimiNotApplicable,
            MessageId::PluginKimiCandidateSummary,
            MessageId::PluginKimiCandidateDetails,
            MessageId::PluginKimiRejectedHeading,
            MessageId::PluginKimiInspectionFooter,
            MessageId::PluginKimiCandidateMissing,
            MessageId::PluginKimiCandidateChanged,
            MessageId::PluginKimiHomeMissing,
            MessageId::PluginKimiRootInspectFailed,
            MessageId::PluginKimiRootMustBeDirectory,
            MessageId::PluginKimiRootCanonicalizeFailed,
            MessageId::PluginKimiRootListFailed,
            MessageId::PluginKimiEntryReadFailed,
            MessageId::PluginKimiEntryLimit,
            MessageId::PluginKimiEntryInspectFailed,
            MessageId::PluginKimiEntryLinksRefused,
            MessageId::PluginKimiEntryOutsideRoot,
            MessageId::PluginKimiEntryCanonicalizeFailed,
            MessageId::PluginKimiManifestUnreadable,
            MessageId::PluginKimiManifestMustBeFile,
            MessageId::PluginKimiManifestInvalid,
            MessageId::PluginKimiDirectoryNameMismatch,
            MessageId::PluginKimiHashUnavailable,
            MessageId::PluginKimiRollbackDestinationMissing,
            MessageId::PluginKimiMismatchRemoved,
            MessageId::PluginKimiMismatchRollbackFailed,
            MessageId::PluginKimiUserPluginDirectory,
            MessageId::PluginKimiMarketplaceZipUnsupported,
            MessageId::PluginKimiMarketplaceRemoteUnsupported,
            MessageId::PluginKimiMarketplaceGzipTarball,
        ];
        let english = raw_locale_messages(Locale::En);
        for locale in Locale::shipped_complete() {
            let pack = raw_locale_messages(*locale);
            for id in ids {
                let key = format!("{id:?}");
                let english_value = english
                    .get(&key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_else(|| panic!("English pack is missing {key}"));
                let translated = pack
                    .get(&key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_else(|| panic!("{} is missing {key}", locale.tag()));
                assert_eq!(
                    message_placeholders(translated),
                    message_placeholders(english_value),
                    "{} changed placeholders for {key}",
                    locale.tag()
                );
                if *locale != Locale::En {
                    assert_ne!(
                        translated,
                        english_value,
                        "{} must translate {key} instead of copying English",
                        locale.tag()
                    );
                }
            }
        }
    }

    #[test]
    fn extensions_modal_has_complete_translated_placeholder_parity() {
        let english = raw_locale_messages(Locale::En);
        let keys = english
            .keys()
            .filter(|key| key.starts_with("Extensions"))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            keys.len(),
            101,
            "the complete extensions locale set changed"
        );

        let prose_keys = [
            "ExtensionsMcpEmpty",
            "ExtensionsMcpBrowse",
            "ExtensionsMarketplaceUnavailable",
            "ExtensionsMcpNotInspected",
            "ExtensionsMcpNotConnectedYet",
            "ExtensionsMcpRefresh",
            "ExtensionsNoItems",
            "ExtensionsNoMatches",
            "ExtensionsProductChromeDescription",
            "ExtensionsProductPlaywrightDescription",
            // The sentence that replaced the word "untrusted" is the most
            // load-bearing prose on the panel: a pack that ships it in
            // English has not localized the one row a person gets stuck on.
            "ExtensionsReviewExplainer",
        ];
        for locale in Locale::shipped_complete() {
            let pack = raw_locale_messages(*locale);
            for key in &keys {
                let english_value = english
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_else(|| panic!("English {key} must be a string"));
                let translated = pack
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_else(|| panic!("{} is missing raw key {key}", locale.tag()));
                assert_eq!(
                    message_placeholders(translated),
                    message_placeholders(english_value),
                    "{} changed placeholders for {key}",
                    locale.tag()
                );
            }
            if *locale != Locale::En {
                for key in prose_keys {
                    assert_ne!(
                        pack.get(key),
                        english.get(key),
                        "{} copied English prose for {key}",
                        locale.tag()
                    );
                }
            }
        }
    }

    #[test]
    fn mcp_capability_metadata_copy_has_complete_locale_parity() {
        let ids = [
            MessageId::McpCapabilitiesAdvertised,
            MessageId::McpCapabilitiesLegacyFallback,
            MessageId::McpCapabilitiesNotObserved,
        ];
        let english = raw_locale_messages(Locale::En);
        for locale in Locale::shipped_complete() {
            let pack = raw_locale_messages(*locale);
            for id in ids {
                let key = format!("{id:?}");
                let english_value = english
                    .get(&key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_else(|| panic!("English pack is missing {key}"));
                let translated = pack
                    .get(&key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_else(|| panic!("{} is missing {key}", locale.tag()));
                assert_eq!(
                    message_placeholders(translated),
                    message_placeholders(english_value),
                    "{} changed placeholders for {key}",
                    locale.tag()
                );
                if *locale != Locale::En {
                    assert_ne!(
                        translated,
                        english_value,
                        "{} must translate {key} instead of copying English",
                        locale.tag()
                    );
                }
            }
        }
    }

    #[test]
    fn tool_receipt_strings_have_complete_locale_parity() {
        let ids = [
            MessageId::ToolReceiptDone,
            MessageId::ToolReceiptLinesSingular,
            MessageId::ToolReceiptLinesPlural,
        ];
        let english = raw_locale_messages(Locale::En);
        for locale in Locale::shipped_complete() {
            let pack = raw_locale_messages(*locale);
            for id in ids {
                let key = format!("{id:?}");
                let english_value = english
                    .get(&key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_else(|| panic!("English pack is missing {key}"));
                let translated = pack
                    .get(&key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_else(|| panic!("{} is missing {key}", locale.tag()));
                assert_eq!(
                    message_placeholders(translated),
                    message_placeholders(english_value),
                    "{} changed placeholders for {key}",
                    locale.tag()
                );
                if *locale != Locale::En {
                    assert_ne!(
                        translated,
                        english_value,
                        "{} must translate {key} instead of copying English",
                        locale.tag()
                    );
                }
            }
        }
    }

    #[test]
    fn mode_picker_strings_are_translated_in_non_english_locales() {
        // The mode hints are full sentences; every shipped non-English locale
        // must provide a real translation rather than leaking the English
        // string through the fallback chain.
        let sentences = [
            MessageId::AppModeAgentHint,
            MessageId::AppModeAutoHint,
            MessageId::AppModePlanHint,
            MessageId::AppModeYoloHint,
            MessageId::AppModeOperateHint,
        ];
        for locale in Locale::shipped_complete() {
            if *locale == Locale::En {
                continue;
            }
            for id in sentences {
                let localized = tr(*locale, id);
                assert!(!localized.is_empty(), "{} empty for {id:?}", locale.tag());
                assert_ne!(
                    localized,
                    tr(Locale::En, id),
                    "{} should translate {id:?}",
                    locale.tag()
                );
            }
        }
    }

    #[test]
    fn zh_hant_hotbar_command_and_keybinding_strings_are_native() {
        for id in [
            MessageId::CmdHotbarDescription,
            MessageId::KbJumpPlanAgentYolo,
            MessageId::KbAltJumpPlanAgentYolo,
        ] {
            let localized = tr(Locale::ZhHant, id);
            assert!(!localized.is_empty(), "zh-Hant empty for {id:?}");
            assert_ne!(
                localized,
                tr(Locale::En, id),
                "zh-Hant should translate {id:?}"
            );
        }
    }

    #[test]
    fn unsupported_locale_falls_back_to_english() {
        assert_eq!(
            resolve_locale_with_env("ar", |_| None),
            Locale::En,
            "Arabic is planned for QA but not shipped in the v0.7.6 core pack"
        );
    }

    #[test]
    fn provider_description_is_present_for_all_locales() {
        for locale in Locale::shipped_complete() {
            let description = tr(*locale, MessageId::CmdProviderDescription);
            assert!(
                !description.is_empty(),
                "{} provider description should not be empty",
                locale.tag()
            );
            assert!(
                !description.contains("codewhale |"),
                "{} provider description should not name codewhale as a backend: {description}",
                locale.tag()
            );
        }
    }

    #[test]
    fn width_truncation_handles_cjk_rtl_indic_and_latin_samples() {
        let samples = [
            ("zh-Hans", "输入以筛选配置"),
            ("ar", "تصفية الإعدادات"),
            ("hi", "सेटिंग खोजें"),
            ("pt-BR", "configurações filtradas"),
        ];

        for (tag, sample) in samples {
            let truncated = truncate_to_width(sample, 12);
            assert!(
                truncated.width() <= 12,
                "{tag} sample overflowed: {truncated:?}"
            );
        }
    }

    #[test]
    fn planned_script_samples_render_in_narrow_terminal_buffer() {
        let samples = [
            ("CJK", "输入以筛选配置"),
            ("RTL", "تصفية الإعدادات"),
            ("Indic", "सेटिंग खोजें"),
            ("Latin Global South", "configurações filtradas"),
        ];

        for (label, sample) in samples {
            let area = Rect::new(0, 0, 18, 4);
            let mut buf = Buffer::empty(area);
            Paragraph::new(sample)
                .wrap(Wrap { trim: false })
                .render(area, &mut buf);
            let dump = buffer_text(&buf, area);

            assert!(
                dump.chars().any(|ch| !ch.is_whitespace()),
                "{label} sample produced an empty render"
            );
        }
    }

    fn buffer_text(buf: &Buffer, area: Rect) -> String {
        let mut out = String::new();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn visible_row_text(buf: &Buffer, area: Rect, y: u16) -> String {
        let mut out = String::new();
        let mut skip_cells = 0usize;
        for x in area.left()..area.right() {
            if skip_cells > 0 {
                skip_cells -= 1;
                continue;
            }
            let symbol = buf[(x, y)].symbol();
            out.push_str(symbol);
            skip_cells = UnicodeWidthStr::width(symbol).saturating_sub(1);
        }
        out
    }

    // --- Unicode / CJK / terminal-width QA (issue #3488) -------------------
    // `truncate_to_width` is the localization-layer truncation helper. These
    // verify it clips by display width (never byte/char count), preserves
    // semantic prefixes, never splits a grapheme cluster, and that mixed
    // English/CJK rows wrap inside a narrow (40-col) and medium (80-col)
    // terminal buffer without overflowing the column.

    #[test]
    fn truncate_to_width_clips_cjk_by_display_width_and_keeps_prefix_intact() {
        // Each Han glyph is two columns. A 12-column budget fits the six-glyph
        // title exactly, so no truncation/ellipsis happens and the prefix survives.
        let title = "项目报告结果"; // 12 columns
        assert_eq!(truncate_to_width(title, 12), title);

        // Oversized: clip on a whole-glyph boundary, append the ellipsis, and
        // stay within the budget by display width.
        let out = truncate_to_width("数据库迁移任务结果", 7); // 10 glyphs = 20 cols
        assert!(
            UnicodeWidthStr::width(out.as_str()) <= 7,
            "{out:?} overflowed"
        );
        assert!(out.ends_with('…'), "expected ellipsis, got {out:?}");
        assert!(!out.contains('\u{FFFD}'), "split a wide glyph: {out:?}");
        // The kept body is whole wide glyphs (each two columns) — never a half cell.
        let body = out.strip_suffix('…').unwrap_or(&out);
        assert!(
            body.chars()
                .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
                .sum::<usize>()
                <= 6,
            "body exceeded budget-minus-ellipsis: {out:?}"
        );

        // A semantic ASCII prefix (e.g. a status verb) survives when it fits.
        let row = "running 数据库迁移任务结果预览测试";
        let out = truncate_to_width(row, 16);
        assert!(
            out.starts_with("running"),
            "semantic prefix dropped: {out:?}"
        );
        assert!(UnicodeWidthStr::width(out.as_str()) <= 16);
        assert!(!out.contains('\u{FFFD}'));
    }

    #[test]
    fn truncate_to_width_never_splits_combining_marks_or_emoji() {
        // Combining mark (U+0301) and ZWJ are zero-width; they must not be
        // counted as columns and must never be cut mid-cluster into U+FFFD.
        let cafe = "cafe\u{0301}"; // "café", 4 columns
        assert_eq!(truncate_to_width(cafe, 10), cafe);
        let out = truncate_to_width("cafe\u{0301} overflow here", 6);
        assert!(UnicodeWidthStr::width(out.as_str()) <= 6);
        assert!(!out.contains('\u{FFFD}'));

        // Emoji is two columns; truncation lands on a cluster boundary.
        let out = truncate_to_width("\u{1F433}\u{1F433}\u{1F433} whales everywhere", 5);
        assert!(UnicodeWidthStr::width(out.as_str()) <= 5);
        assert!(!out.contains('\u{FFFD}'));
    }

    #[test]
    fn narrow_and_medium_terminal_wraps_mixed_width_rows_without_overflow() {
        // Issue #3488 acceptance: at a 40-col (narrow, macOS-Terminal-like) and
        // 80-col (medium) terminal, mixed English/CJK task titles and transcript
        // lines must (a) truncate to the column by display width, and (b) wrap
        // inside the buffer so no rendered row exceeds the terminal width.
        let fixtures = [
            "Task: 数据库迁移任务 — verify provider routing for issue #3488",
            "抹香鲸 is running codex/issue-3439-zhipu-glm-fixture @ issue-3439",
            "满員電車🫠 — full-width punctuation：『』【】 mixes with ASCII ids",
        ];

        for width in [40usize, 80] {
            // (a) The truncation helper clips by display width.
            for fixture in fixtures {
                let out = truncate_to_width(fixture, width);
                assert!(
                    UnicodeWidthStr::width(out.as_str()) <= width,
                    "width={width}: truncated row overflowed: {out:?}"
                );
                assert!(
                    !out.contains('\u{FFFD}'),
                    "width={width}: split a glyph: {out:?}"
                );
            }

            // (b) Wrapping the full mixed-width line inside a buffer of `width`
            // columns never lets a rendered row exceed the terminal width.
            for fixture in fixtures {
                let area = Rect::new(0, 0, width as u16, 6);
                let mut buf = Buffer::empty(area);
                Paragraph::new(fixture)
                    .wrap(Wrap { trim: false })
                    .render(area, &mut buf);
                let mut saw_text = false;
                for (row_idx, y) in (area.top()..area.bottom()).enumerate() {
                    let row = visible_row_text(&buf, area, y);
                    let trimmed = row.trim_end_matches('\u{0}').trim_end();
                    assert!(
                        UnicodeWidthStr::width(trimmed) <= width,
                        "width={width} row {row_idx}: wrapped row overflowed ({} cols): {trimmed:?}",
                        UnicodeWidthStr::width(trimmed)
                    );
                    saw_text |= trimmed.chars().any(|ch| !ch.is_whitespace());
                }
                assert!(
                    saw_text,
                    "width={width}: mixed fixture produced an empty render"
                );
            }
        }
    }

    // --- Cyrillic script fixtures (ru/uk, #3092 / #4791) -------------------
    // Russian and Ukrainian share the Cyrillic script but are different
    // languages. These fixtures lock the failure modes seen in real
    // machine-translated packs: Russian-only letters (ы/э/ъ) leaking into
    // the Ukrainian pack, Ukrainian-only letters (і/ї/є/ґ) leaking into the
    // Russian pack, untranslated English prose hiding behind the fallback,
    // and one pack copied into the other.

    fn has_cyrillic(value: &str) -> bool {
        value
            .chars()
            .any(|ch| ('\u{0400}'..='\u{04FF}').contains(&ch))
    }

    fn has_devanagari(value: &str) -> bool {
        value
            .chars()
            .any(|ch| ('\u{0900}'..='\u{097F}').contains(&ch))
    }

    /// Latin words remaining after the exempt categories are stripped:
    /// `code spans`, {placeholders}, URLs, slash commands, env-style
    /// ALL-CAPS tokens, and the product-term allowlist from
    /// `locales/AGENTS.md`. Anything left over in a Cyrillic or Devanagari
    /// string is mixed-language copy.
    fn latin_words_in_translated_copy(value: &str) -> Vec<String> {
        const ALLOWED: &[&str] = &[
            "codewhale",
            "deepseek",
            "fleet",
            "plan",
            "act",
            "operate",
            "ask",
            "auto",
            "review",
            "full",
            "access",
            "enter",
            "esc",
            "alt",
            "ctrl",
            "shift",
            "tab",
            "space",
            "backspace",
            "delete",
            "api",
            "json",
            "toml",
            "yaml",
            "yml",
            "tui",
            "ci",
            "cd",
            "mcp",
            "url",
            "uri",
            "dns",
            "ssh",
            "http",
            "https",
            "git",
            "github",
            "gitee",
            "openai",
            "anthropic",
            "gemini",
            "kimi",
            "codex",
            "claude",
            "vllm",
            "ollama",
            "sglang",
            "npm",
            "rust",
            "cargo",
            "linux",
            "macos",
            "windows",
            "id",
            "ok",
            "true",
            "false",
            "utf",
            "ascii",
            "cli",
            "ui",
            "md",
            "ai",
            "llm",
            "gpt",
            "faq",
            "docs",
            "admin",
            "oauth",
            "ssl",
            "tls",
            "jwt",
            "svg",
            "png",
            "wasm",
            "app",
            "slash",
            "skill",
            "plugin",
            "shell",
        ];
        let mut scrubbed = String::with_capacity(value.len());
        let mut chars = value.chars();
        let mut in_backtick = false;
        let mut in_brace = false;
        for ch in chars.by_ref() {
            match ch {
                '`' => in_backtick = !in_backtick,
                '{' if !in_backtick => in_brace = true,
                '}' if in_brace => in_brace = false,
                _ if !in_backtick && !in_brace => scrubbed.push(ch),
                _ => {}
            }
        }
        scrubbed
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '/')
            .filter(|token| token.len() >= 2)
            .filter(|token| !token.contains('/') && !token.contains("://"))
            .filter(|token| token.is_ascii())
            .filter(|token| !token.chars().any(|c| c.is_ascii_digit()))
            .filter(|token| !token.chars().all(|c| c.is_ascii_uppercase()))
            .filter(|token| !ALLOWED.contains(&token.to_ascii_lowercase().as_str()))
            .map(str::to_string)
            .collect()
    }

    /// High-visibility chrome where mixed-language copy is most visible.
    const SCRIPT_FIXTURE_IDS: &[MessageId] = &[
        MessageId::ComposerPlaceholder,
        MessageId::HistorySearchTitle,
        MessageId::HistorySearchPlaceholder,
        MessageId::StatusPickerTitle,
        MessageId::StatusPickerInstruction,
        MessageId::ConfigTitle,
        MessageId::CommandPaletteTitle,
        MessageId::AppModeAgentHint,
        MessageId::AppModePlanHint,
        MessageId::RouteNoModels,
        MessageId::ProviderNoMatchesTitle,
        MessageId::SessionsOpenedHistory,
    ];

    #[test]
    fn cyrillic_packs_have_script_purity_and_no_mixed_language_fixtures() {
        for locale in [Locale::Ru, Locale::Uk] {
            let messages = raw_locale_messages(locale);
            let total = messages.len();
            let with_cyrillic = messages
                .values()
                .filter(|v| v.as_str().is_some_and(has_cyrillic))
                .count();
            assert!(
                with_cyrillic * 100 >= total * 85,
                "{}: only {with_cyrillic}/{total} values contain Cyrillic — pack looks under-translated",
                locale.tag()
            );
            for (key, value) in &messages {
                let Some(value) = value.as_str() else {
                    continue;
                };
                if locale == Locale::Uk {
                    assert!(
                        !value.chars().any(|c| "ыэъЫЭЪ".contains(c)),
                        "uk {key} contains a Russian-only letter: {value}"
                    );
                } else {
                    assert!(
                        !value.chars().any(|c| "іІїЇєЄґҐ".contains(c)),
                        "ru {key} contains a Ukrainian-only letter: {value}"
                    );
                }
            }
            for id in SCRIPT_FIXTURE_IDS {
                let value = tr(locale, *id);
                assert!(
                    has_cyrillic(&value),
                    "{} {id:?} fixture has no Cyrillic: {value}",
                    locale.tag()
                );
                let leaked = latin_words_in_translated_copy(&value);
                assert!(
                    leaked.is_empty(),
                    "{} {id:?} mixes Latin prose into Cyrillic copy: {leaked:?} in {value}",
                    locale.tag()
                );
            }
        }
        // The two packs are translations of the same source, not copies of
        // each other: sentence-length fixtures must differ between ru and uk.
        for id in [
            MessageId::ComposerPlaceholder,
            MessageId::StatusPickerInstruction,
            MessageId::AppModeAgentHint,
            MessageId::AppModePlanHint,
            MessageId::ProviderNoMatchesTitle,
        ] {
            assert_ne!(
                tr(Locale::Ru, id),
                tr(Locale::Uk, id),
                "ru and uk share an identical sentence for {id:?} — one pack was copied from the other"
            );
        }
    }

    #[test]
    fn hindi_pack_uses_devanagari_for_prose_fixtures() {
        let messages = raw_locale_messages(Locale::Hi);
        let total = messages.len();
        let with_devanagari = messages
            .values()
            .filter(|v| v.as_str().is_some_and(has_devanagari))
            .count();
        assert!(
            with_devanagari * 100 >= total * 80,
            "hi: only {with_devanagari}/{total} values contain Devanagari — pack looks under-translated"
        );
        for id in SCRIPT_FIXTURE_IDS {
            let value = tr(Locale::Hi, *id);
            assert!(
                has_devanagari(&value),
                "hi {id:?} fixture has no Devanagari: {value}"
            );
            let leaked = latin_words_in_translated_copy(&value);
            assert!(
                leaked.is_empty(),
                "hi {id:?} mixes Latin prose into Devanagari copy: {leaked:?} in {value}"
            );
        }
    }

    #[test]
    fn no_shipped_locale_renders_a_missing_message_marker() {
        // rust_i18n falls back to en for absent keys, so a "{MessageId}"
        // debug string in the UI would mean the fallback chain itself broke.
        for locale in Locale::shipped() {
            assert!(
                missing_message_ids(*locale).is_empty(),
                "{} renders raw message ids (missing-marker UI)",
                locale.tag()
            );
        }
    }

    // --- Devanagari grapheme safety (#4790 spike) --------------------------

    #[test]
    fn truncate_to_width_never_splits_devanagari_clusters() {
        // क्ष is क + ् + ष — a single cluster. A budget landing inside it
        // must drop the whole cluster; a dangling virama (U+094D) renders as
        // visibly broken shaping (क् instead of a conjunct).
        let conjuncts = "क्षत्रिय ज्ञान श्रृंखला प्रत्यक्ष";
        for budget in [1usize, 2, 3, 5, 7, 40, 60, 80] {
            let out = truncate_to_width(conjuncts, budget);
            assert!(
                UnicodeWidthStr::width(out.as_str()) <= budget,
                "budget={budget}: overflowed: {out:?}"
            );
            assert!(!out.contains('\u{FFFD}'), "budget={budget}: {out:?}");
            let body = out.strip_suffix('…').unwrap_or(&out);
            assert!(
                !body.ends_with('\u{094D}'),
                "budget={budget}: dangling virama: {out:?}"
            );
            assert!(
                !body.ends_with('\u{200D}'),
                "budget={budget}: dangling ZWJ: {out:?}"
            );
            if let Some(last) = body.chars().last() {
                let cp = last as u32;
                let combining = (0x0900..=0x0903).contains(&cp) || (0x093A..=0x094F).contains(&cp);
                assert!(
                    !combining,
                    "budget={budget}: trailing combining mark: {out:?}"
                );
            }
        }
    }

    #[test]
    fn cyrillic_latin_extended_and_devanagari_rows_wrap_within_terminal_columns() {
        // Width/grapheme QA for the v0.9.2 scripts at narrow (40), medium
        // (60), and standard (80) terminal columns: truncation clips by
        // display width and wrapped rows never overflow the buffer.
        let fixtures = [
            (
                "ru",
                "Задача: миграция базы данных — проверка маршрутизации провайдера #3092",
            ),
            (
                "uk",
                "Завдання: міграція бази даних — перевірка маршрутизації провайдера #4791",
            ),
            (
                "de",
                "Aufgabe: Datenbankmigration — Anbieter-Routing für #4788 prüfen",
            ),
            (
                "fr",
                "Tâche : migration de la base — vérifier le routage fournisseur #4788",
            ),
            (
                "ca",
                "Tasca: migració de la base de dades — comprovar l'encaminament #4788",
            ),
            (
                "id",
                "Tugas: migrasi basis data — periksa perutean penyedia untuk #4789",
            ),
            ("hi", "कार्य: डेटाबेस माइग्रेशन — प्रदाता रूटिंग की जांच करें #4790"),
        ];

        for width in [40usize, 60, 80] {
            for (tag, fixture) in fixtures {
                let out = truncate_to_width(fixture, width);
                assert!(
                    UnicodeWidthStr::width(out.as_str()) <= width,
                    "{tag} width={width}: truncated row overflowed: {out:?}"
                );
                assert!(
                    !out.contains('\u{FFFD}'),
                    "{tag} width={width}: split a glyph: {out:?}"
                );

                let area = Rect::new(0, 0, width as u16, 6);
                let mut buf = Buffer::empty(area);
                Paragraph::new(fixture)
                    .wrap(Wrap { trim: false })
                    .render(area, &mut buf);
                let mut saw_text = false;
                for (row_idx, y) in (area.top()..area.bottom()).enumerate() {
                    let row = visible_row_text(&buf, area, y);
                    let trimmed = row.trim_end_matches('\u{0}').trim_end();
                    assert!(
                        UnicodeWidthStr::width(trimmed) <= width,
                        "{tag} width={width} row {row_idx}: wrapped row overflowed ({} cols): {trimmed:?}",
                        UnicodeWidthStr::width(trimmed)
                    );
                    saw_text |= trimmed.chars().any(|ch| !ch.is_whitespace());
                }
                assert!(
                    saw_text,
                    "{tag} width={width}: fixture produced an empty render"
                );
            }
        }
    }
}
