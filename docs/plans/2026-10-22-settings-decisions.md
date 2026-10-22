# Settings implementation decisions

These are historical implementation rulings in their original order, retaining
the reasons and stated costs. Later clarifications supersede earlier wording.
The user's final direction stopped delegation; the primary completed the
remaining code, testing and review directly. Excessive task splits and repeated
review cycles contributed to the delay. References to temporary task reports in
this historical record are superseded by the retained verification and action
coverage documents, which name concrete tests.

## Decision 1

Ruling: use managed worktree outside main and request automatic sandbox approvals for writes there — required isolation and authorized implementation — costs only reversible local branch/worktree.

## Decision 2

Ruling: implementers run focused tests during task development; full ordered fmt/clippy/test before each task commit where feasible, with explicit evidence — repository requirement — extra validation time.

## Decision 3

Ruling: starting mode stores provider-native optional mode ID, not Build/Plan enum — native permissions have provider-specific IDs and dynamic support — avoids silently remapping permissions.

## Decision 4

Ruling: SessionSort defaults Manual — current ui.rs:989 sorts by explicit session_order then creation ID and never changes on activity — preserves incumbent behavior; Active first is an opt-in choice despite mock example.

## Decision 5

Ruling: Settings saves preserve unknown top-level and nested JSON fields as well as known unrelated config — patch-only contract includes data from newer versions — requires typed edits plus raw preservation at storage boundary, not arbitrary editable JSON. Task1 asked to add fixture and preserve unknown fields before review.

## Decision 6

Ruling: split largeTask3 into3a refresh/appearance/motion;3b conversation/layout/input/timestamps;3c diffpresentation/context — separate production subsystems and review surfaces — no scope reduction, all3 required beforeTask3 complete. Fresh implementers per slice after scoped reviews. Current /root/settings_rendering owns3a only; task-3a-brief.md supplied. Basebc2f480. Legacyreplycps0 gates also affectsidebar/screen;3a must explain compatible effectivepolicy/migration.

## Decision 7

Ruling: live auto-scroll policy remains committed until Save; draft policy previews only in the synthetic sample using actual transcript Window — previewingAlways on liveunderlay could irreversibly move readingposition and violate preserve-scroll/Discard guarantee — cost ifwrong: usermustSave totry livefollow, while safe sample remainsimmediate. Task3b adds incomingentries+Discardpreservesanchor/draft/copy andSaveappliespolicy tests.

## Decision 8

Ruling: retain existing entry/unclamped readingoffset across preview and committed formatting/height clamps while notfollowing; explicitnavigation or intentionalfollow replaces it, missingentryfallsbacksafely — preservesreadingintent consistentlywithoutSettings-onlysnapshots — costifwrong: re-expanding anentrywithoutinterveningnavigation returnsits oldreadingposition ratherthanclampedheading. Task3b tests Discard+incomingcontent and explicitnavigationafterSave.

## Decision 9

Ruling: add minimalreadonly ChangesContext request carryingcanonicalpatch plus existingSource/serialguard, verifyingpatchbefore/after contextread; preserveoldDiffonmismatch — patch-onlyprotocolcannotimplement5/10 context — costifwrong:newprotocolsurface mustremainbackwardcompatibleand tested. ReuseGit/snapshot semanticsforPRpinnedrefs(includinguncachedfetch),renames,untrackedfiles,symlinks; context-onlymappingmustnotreplacecanonicalhunks.

## Decision 10

Ruling: Changes willhonor existingcanonical config.unified justlikeReviews, including splitwhenfalseandwidthallows; persisteddefaultsunchanged — priorChangesignoredthissharedpreference andallviews mustnowhonor it — costifwrong: defaultChangesappearancechangesfromsinglecolumntosplit; userscanchooseUnified.

## Decision 11

Ruling: allow a narrow cfg(test)-only scoped thread-local process adapter for the complete uncached-PR ChangesContext fixture, reusing an existing seam if available. Fake only exact fixture gh responses and rewrite expected GitHub fetch transport to a local remote; actual Service::handle, clone validation, before/after patch checks, missing-revision fetch, snapshot and source mapping stay real. Require RAII cleanup, exact command assertions, other commands unchanged, explicit thread scope, revision absent-before/present-after proof; no global PATH/HOME/auth change. Reason: validate the new end-to-end orchestration without a live authenticated account. Cost if wrong: fixture transport assumptions could differ from GitHub, so live provider transport remains an explicit limit.

## Decision 12

Ruling: split Task4 into4a full registry/catalog + locked save/presets + Shell/shared palette/Settings wiring,4b all Agents dispatch/overlays/question/send/voice,4c all Reviews/workflow/local/hover/overlays and whole action inventory audit. Initial inventory:5 Shell global actions,~35 Agents and~40 Reviews families,~25 modal/picker handlers plus direct numeric choices. Fresh sequential implementers after each review; no parallel shared-file edits. Reason: three distinct dispatch ownership surfaces; cost if wrong: more integration passes, but no scope is dropped and wholeTask4 remains pending until all complete. Existing /root/settings_shortcuts now owns4a, report task-4a-report.md. Approved typed ActionId/Context constants and exclusive atomic context sets with central Bindings::new(&Config); full Config is needed for send companions/selection override. Preset writes all three companion overrides atomically, raw conflict snapshots separate from Custom display.

## Decision 13

Ruling: split4b into4b1 currentmain/composer/question/voice/copy/navigation+theirhints/tester and4b2 remainingAgents overlays/forms/helpers/contexts/hints/finalaudit. Implementercheckpoint has~37concreteIDs and remainingpermissions/MCP/models/defaults/rules/remote/Slack/resources/helper/queue/plan as substantialindependentpass. Reason: review coherent core lifecycle before expanding remaining consumers; costifwrong: additionalreview/integrationpass, no scope drop. Whole4b remains pending untilbothcomplete. /root/settings_agents_shortcuts owns4b1, base74d69ba, reporttask-4b1-report.md. Freshsequential4b2 afterreview. Briefs4b1/4b2written; mainactivehints andexactpendinginventory requiredbeforecheckpoint.

## Decision 14

Ruling:4b1 preserve legacy nonShift Enter question-submit aliases including Hyper/Meta via narrow default-only semantic recognition in authoritative resolver if needed, without broadening persisted/capturable KeyChord schema. Active only in actual question contexts, disabled by any custom question.submit override, neverRelease/ShiftEnter; no rawevent rewriting. Reason: oldhandler accepted these events but currentcapture intentionallyrejects opaque modifiers. Costifwrong: one compatibility predicate requires maintenance; tests must prove override removal and editing safety. Eight representable nonShift combinations are catalog aliases. Plan/inlinequestion earlypriority stays unchanged.

## Decision 15

Ruling: split4b2 into4b2a permissions/MCP(26specificIDs),4b2b models/defaults/rules+repositoryselectors(18),4b2c remainingoverlays/helpers/finalAgentsaudit(13plusremainingsharedcontexts). Implementer escalated57IDs/~36contexts acrossmanymodules asomissionrisk before sourcework. Sequentialfreshimplementers/reviews preserveentirescope. Costifwrong: extrareview/checkpointtime; nofeaturedeferredoutsideapprovedtask. /root/settings_agents_overlays nowowns4b2a only, basecbac54d, reporttask-4b2a-report.md. Otherbriefswritten; whole4b2 pendingall3.

## Decision 16

Ruling: remove provisional editable mcp.choose_5..9 actions, not expose5inertsettings. Verified actualMCP State::actions maximum4:tools+oneexclusiveauth+reconnect+toggle. Legacy5..9 remainno-op; unknownstoredIDs preservedopaquely. Reason: approvedregistrycoversrealactions and nofakecontrols; costifwrong: futurefifthMCPactionrequiresnewregistration.4b2a mustshowmax4capability/no-op tests andcorrectinventorycounts. Approvednarrowcfg(test)URLlauncherboundary afterrealvalidation/cancel, uniquefixtureURL+boundedchannel/RAII, failclosedtestnamespaceafterdrop topreventlateasyncrealbrowserlaunch; noproductionbehavior/envchange.

## Decision 17

Ruling: keep intentionalcapturable/defaultaliaslist for4b2/4c; do not expand every ignoredmodifier in legacyKeyCode-onlynoneditorhandlers intoimplicitdefaultaliases. Preserve documented/explicitmeaningfulbehavior,CtrlEnterpaths,terminalnormalization,modalEsc,rawEditor/browser,earlyglobals andreviewedquestionexplicitnonShiftcompatibility. Reason: arbitrarycatchalls overlapglobal/customactions and cannottruthfullysupportcontextconflictvalidation withoutmassiveimplicitmap. Costifwrong: rareundocumentedmodifiervariants(e.g CtrlDown inlist) maystopacting untiluserexplicitlyassignsthem. Agentaskedbeforeinventingpolicy; tests/docs muststateboundary.

## Decision 18

Ruling: remove provisional approval.submit_text and AGENT_APPROVAL_TEXT as unreachable advertisedcontrols. Controllerverified inline_question matchesallrequestUserInput approvals; shortcut_context choosesQUESTION/NOTE first and question_key consumesbeforemodal_key. Preserveactualinline QUESTION_SUBMIT, no secondtextapprovalfeature. ActualUi::key text/options/note reachability andunknownoverridepreservation required; inventory/counts updated by4b2c. Reason: registry mustdescribe real actions, like priorinertMCPslots. Costifwrong: futureseparatetextapprovalpath requiresnewexplicitregistration; opaqueoldstoredID survives. Task4b2c owns12specificrealIDs aftercorrection, remaininggenericapproval stilllive.

## Decision 19

Ruling: prune selection.copy catalogmemberships/defaultaliases onlyforprovennoncopyableAgentsmodalstates (no focusedEditor andnonTranscript). Controllerverifiedcopy_chat_selection/selection_mouse guards; keepactualEditor/Transcript/main/helper consumers andphysicalCtrlC-beforequit. No newclipboardfeature; hiddencomposer guard/no-op remainsrequired, existingstoredglobalbindingunchanged. Reason: accurateeffectivecontexts/conflictmetadata ratherthanadvertisinginertcopy. Costifwrong: futuresupportedselectioninpreviouslynoncopyablemodal requiresexplicitmembership; reviewexactremovedcontextlist.

## Decision 20

Ruling clarification: retain4b2c focusedEditor copyrepairs forTweaks andunmaskedSlackmember/channelfields; theseexistingEditors alreadyallowtextselection butwereomittedfromfocused_editor. No-new-clipboard-feature ruling prohibitsinventingselectiononwholemodalbodies/lists, notmakingstandardcopyhonorrealEditorownership. MaskedSlacktokensremainexcluded; exactselectedtext/correctfield/hiddencomposer/button/physicalCtrlC regressions required. Reason: sharedcopy/remap+editingconventionsneedtruthfulfunctionalEditorconsumers. Costifwrong: selectedtextcopybecomesnewlysupportedinthesepreviouslyomittedfields; noautomaticcopy, newsecretsurface orselectablemodalbody. Reportdeliberatecorrection, notlegacyclaim.

## Decision 21

Ruling: retainone dedicated overlay_shortcut_tests.rs (~1656lines) ratherthanarbitrarytestsplit; renamefromtemporalremaining_shortcut_tests.rs fordurabledomainname. Sharedtestfixtures/table-drivencross-modal invariants arecoherent andsimilarsizetoexisting1600line shortcut_tests; independentreviewstilljudgesmaintainability. Costifwrong: largernewtestfileislessnavigableandmayneedfuturefocuseddecomposition; no productionrestructure.

## Decision 22

Ruling: preserveoriginalfocusedhelperavailability byremovingAgentHelper from agents.models/question.open metadata; oldpanel_key onlypassedsidebar/Changes toearlymainhandlers. Literal/default/customno-opwithretaineddraft/anchor required, otheractualmaincontexts remainworking. Reason: no silentdefaultbehavior expansion; costifwrong: usersmustleavehelperfocusbeforetheseactions, asbefore, ratherthangainingnewhelperavailability.

## Decision 23

Ruling: refineHelperMain/BrowserMain contexts to preserveplacement-specificlegacyfocusrouting. Controllerverifiedaa8cd09panel_key: sideTab/BackTab handledbeforebrowser, mainreadonlyhelperTabonly, mainBrowserrawTab/BackTab; settings.rs makesRight+Bottom panels.right=true. ExistingHelper/Browser describeside, newMaincontexts excludeunsupportedhostfocusactions; ShellusesactualUi context/allbrowserguards coverboth inclCtrlC. ActualRight/Bottom/Main default/custom/rawfallthrough required; embeddedPRApp semanticfocus/filters handoff4c. Reason: onecontextcannottruthfullymodeldistincthost/browserownership. Costifwrong: twoadditionalcontexts requiremaintenanceandmayincorrectlyinterceptbrowserkeys; exactprotocol tests/reviewmitigate.

## Decision 24

Ruling:4c separates actualinputcontext forShellglobals frominternalAgentshelperdispatch context. Controllerverifiedpanel_key369-449: embeddedAppmodal/filter consumesbeforehost; plaincontent hostplacement/visibility/focus/back/sidebar/Changes precedesApp. Centralizedaction-awareembeddedcoavailability checking inBindingsconflicts mustrejectcustomcollisions acrosscoactivehost/Reviewcontexts, includingfinalmergedsave, whilekeepingintentionaldefaultEscape/focusprecedence anddisjointmodal/filter assignments. Reason: onesinglecontext cannotexpressbothactualeditingownerandhostprecedence withoutcloningallAppcontexts. Costifwrong: coavailabilitymetadata canoverrejectbindingsorshadowactions; actualRight/Bottom/Main dispatch,capture/concurrentlocked-save andnormalization tests+reviewrequired.

## Decision 25

Ruling:4c prunesonlyReviewcopymembershipsprovenunavailablebyactualcopy_diff/SuperCguards (exactsetinfinalreport); ReviewHelpHome/EndremainqueryEditorconventions unlikeDefinition scrolling. Reason: registrymustdescribeexistingreachablebehavior, noinventedmodalbodycopy/scroll tojustifyprovisionalmetadata. Costifwrong: usersloseanadvertisedbutineffectiveassignment inthosecontexts; futureselectableconsumersneedexplicitregistration. PreserveactualEditor/diff/Controls/Composecopy,CtrlCpriority andliteralno-op/sourceintenttests.

## Decision 26

Ruling: extendexistingcfgtestprocess::fixture withexplicit scopedhandleinheritance atApp::spawn for4c asyncRevieweffecttests. ControllerverifiedACTIVEthreadlocal synchronousadapterandworkerboundary; currentfixturecannotobserveworkergh/open/providertransport. Keepper-test isolation/RAIIrestoration/failclosedunmatchedexternalcommandsinclparentcleanup/exactargs+writeinput/boundedworkerjoin; noprocessglobalenvorproductionbehaviorchanges, realtempGitanduncachedPRfixturespreserved. Reason: actualReviewwrites/link/refresh testsneedrealreducer/worker-tofakeexternalboundaryratherthanflagonlyproof. Costifwrong: testtransportcouldmaskconcurrencyorleakrealcommands; explicitisolation/mismatch/cancellation testsandreviewrequired.

## Decision 27

Ruling:4c WorkerScope suppliescfgtest-only isolatedtemp-root accessor forworktreeslist/delete(andrelevantcreation), same rootforinventoryandparentguard. Controllerconfirmedworktrees.rslist scansstdtemp andopens/createsdifu.lock; deletechecksstdtemp parent. Noprocessenvmutationorproductionpathchange; ownedtempfixturesonly, outside-root/sibling/lock/marker/cancelguardtestsrequired, preserveexisting/private/tmp/difu-whatif. Reason: realworkflowworktreeactiontests mustnotenumerateorlockuserworktrees. Costifwrong: fixturecouldhidetemp-root mismatch; sameaccessorandrealguardtests+reviewmitigate.

## Decision 28

Ruling: extend4c WorkerScope with exact-expected localGit execution onlyafterfullargs/inputmatch andresolved -C repositorywithinownedisolatedroot; noblanketgitallowlist orproductionchange. Unmatchedremote/open/providercommands remainfailclosed. Reason: positiveworkflowdelete-confirm/prune mustactuallyremoveownedtestworktreesandmetadata, notonlyassertguardfailure; currentstrictfake rejectsallsubprocesses. Costifwrong: testallowancecouldexecunintendedGitcommand; exactexpectations/rootchecks/refusaltests andreviewrequired. Separateexistingsynchronousfixturepreserved.

## Decision 29

Ruling: preserveoriginalReview-only explicitControlCquitpredicate withprecisecontextaliases/precedence, notclassifyitasincidentalKeyCode-onlyvariant. OriginalAppchecksCharc+containsCONTROL afterSupereditorcopy andlocal-keyhandling; preservethoseexclusionsincludingnoselectionSupereditorconsumption, customquitremovesallactionaliases,Agentsdefaultunchanged. Reason: consistentpreviousmodifierpolicyandexistingReviewbehavior. Costifwrong: raremodifiedCtrlCcouldexitunexpectedly; literalcontext/alias/removal/opaque/CtrlCprioritytestsandreviewrequired. HelpnonShift/nonSuperCtrl/Altarrowaliaseslikewisepreservedfromexplicitpredicate.

## Decision 30

Ruling: accept service-owned async dispatcher with lightweight transitions queued after sessionslock, persistent at-most-once consumedmarkers in dedicatednotificationssubdirectory, bounded inappfeed, monotonic6sclientleases/2srenewal independentofinputidle. Markbeforeexternaldelivery topreventreplay; deliveryfailure isvisible andexplicitTestcanretry, noautomaticold-eventretry. Defineoverlay/helper viewed-sessionsemantics fromactualconversationvisibility, notrememberedselection. Reason: approvedservicebackgrounddelivery/noreplay requiresdistincteventandpresenceownership. Costifwrong: crashbetweenmarkerwriteanddeliverycanloseonealert; boundedfeedmaydropolderactivity, leaseexpirycanbrieflymisclassifyfocus. Testsanddocs muststatepolicy.

## Decision 31

Ruling: Linuxstandaloneaudio mayuseoptionalpaplay withgeneratedshortWAV; desktopnotify-rust alwayssuppressplatformsound toavoiddoubleaudio andInApp+sound nevercreatesdesktoptoast. Missingexecutable/audio-server/playbackfailure mustshowunavailable/errorwithoutchangingprefs; previewandrealuse sameboundedbackend. Noautomaticdependencyinstall. macOSbundledhelper usesUserNotifications plusNSSound, explicitminmacOS11.0, stdinJSON/executable-relative lookup. Reason: freedesktopNotifytiesaudio todesktoptoast; standaloneaudiohonorsindependentsettingandmuslCLIcompatibility. Costifwrong: Linuxsounddepends onpaplay/audio-serveravailability; UI/docs mustshowthisactualcapability ratherthanpromiseuniversalplayback. Actualaudibilitynotclaimedfromfaketransporttests.

## Decision 32

Ruling: Guide/Conflict completion withoutproviderturnID usespersistedper-sessiongeneration advancedonlyrealnonCompleted->Completedtransition; providercompletion requireschangednonemptycompleted_turn andactualsuccessfulIdleorCompleted, failureonlyerror, Interruptedneither. Seedafterservicerestore andconsumeeventidentity independentofcurrentdeliveryprefs. Reason: actualjobtransitionsneedstableidentitywithoutinventedturnIDs. Costifwrong: repeatedlegitimatecompletionwithoutinterveningstateedgecannotbedistinguished; persistedgenerationmustnotreset/replayafterrestart.

## Decision 33

Ruling: explicitnotificationTestusesdraftchoiceswithoutSave/sessionmutation andrespectsQuiet/soundOff/disabledeventchoices, displayingwhysuppressed. NativeauthorizationonlywhenanexplicitTestactuallyrequestsunsuppressednativeoutput; InApp/sound-only/suppressedTestcannotrequestnotificationpermission. Reason: try-before-savingmustpreviewselectedbehaviorandQuietmustremaintrustworthy. Costifwrong: hearingasoundwhileQuietisonrequiresdraftQuietofffirst; visibleexplanationmustpreventapparentlybrokenTest.

## Decision 34

Ruling: add persisted Session.completed_turn_successful bool (serde default false), assigned alongside completed_turn from the actual provider result. Controller verified Claude Decoder cancellation with send_next=true deliberately yields Idle and takes turn_id into completed_turn, so successful-terminal-status inference is insufficient. Preserve existing status and plan behavior; reset the marker at server/provider completion-reset paths and retain exact outcome across deferred Claude finish. Cover real cancellation send_next true/false, failures, later success, unknown/missing Codex status, legacy JSON. Reason: completion notifications must represent success, not cancellation. Cost if wrong: new durable field adds schema maintenance; old records lack explicit success and remain false, while startup baseline prevents historical alerts anyway. No UI lifecycle reinterpretation is authorized.

## Decision 35

Ruling: Task6 applies explicit new-session provider and pending native mode immediately after Session::new and before rules capture/persistence/worker start; raw Launch retains legacy inference. Reuse actual provider acknowledgement before the first turn, without conflating permission mode and workspace isolation. Reason: defaults must affect the first request, never mutate existing sessions. Cost if wrong: initial session launch may fail visibly when a selected native mode cannot be acknowledged; tests must prevent silently sending the first turn under another mode.

## Decision 36

Ruling: Task6 uses SettingsDraft dependency guards for provider/model/effort/mode and captured baselines in existing model/default managers. A deliberately selected model sets its inferred provider and clears an incompatible native mode when the provider changes; patch only edited fields, preserve unrelated concurrent changes, no network under lock. Reason: explicit defaults must interoperate with legacy managers without producing mixed-provider bundles. Cost if wrong: selecting a different-provider model can clear an old provider-specific mode; UI and tests must make resulting new-session behavior explicit.

## Decision 37

Ruling: explicit provider sign-in temporarily suspends the TUI and delegates interaction to the native CLI with inherited foreground stdin/stdout/stderr, preserving Shell/Panel draft in memory. Separate SIGINT cancellation state from permanent HUP/TERM termination; consume/reset SIGINT only for auth cancellation, retain ordinary Difu SIGINT exit otherwise. Parent observes/cancels/reaps the auth child with bounded cleanup, restores terminal flags/raw/alternate screen on every path, reports notification presence away and refreshes sanitized status on return. Controller confirmed main currently combines these signals, which would otherwise close Difu on auth Ctrl+C. Reason: native interactive login stays usable without capturing credentials or corrupting active TUI output. Cost if wrong: login temporarily occupies the terminal; signal/cleanup mistakes could close Difu, leave raw mode wrong or leak a child. Fake executable/PTY tests for success, cancel, spawn failure and HUP/TERM plus preserved draft/terminal state are required. No real login is authorized as a test.

## Decision 38

Ruling clarification: preserve existing lazy startup for empty new sessions; keep native starting mode pending until provider initialization and acknowledge it before the first user turn. Extend existing Codex model discovery with collaborationMode/list and use actual permissions::snapshot/repository restrictions for Claude. Reason: dynamic native capabilities must reflect the provider without creating a model turn or forcing authentication merely to browse settings. Cost if wrong: mode errors can surface on first message rather than empty creation; visible error and no wrong-mode turn tests are required.

## Decision 39

Ruling: add directional repository/native-mode optimistic dependencies: selecting a non-Automatic starting mode, or editing provider/model while retaining it, guards repository; editing repository guards starting_mode. Derive this from final draft intent, so clearing a mode to Automatic (including a model/provider switch that clears an incompatible mode) may still merge an independent concurrent repository edit. Reason: native mode availability is repository-keyed, so two windows must not save a capability-invalid combined default bundle. Cost if wrong: legitimate concurrent repo/mode changes may require retry after conflict; overly sticky guards could reject independent Automatic-mode edits. Both save orders and clear-to-Automatic merges require regression evidence. No network/capability lookup under the config lock.

## Decision 40

Ruling: retry Interrupted reads within the existing service frame, retaining already received bytes and never resending the request. Reason: the final emulated Linux run exposed interrupted reads that closed valid service connections and caused broken-pipe failures in action tests; a deterministic regression reproduced the failure before the fix. Cost if wrong: retrying non-transient errors could hide real disconnects, so the regression also requires EOF, timeout, WouldBlock and reset failures to remain failures. The retry is limited to Interrupted and the existing frame-size bound is unchanged.
