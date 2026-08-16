//! One answer to "is this family set up before the ship leaves?".
//!
//! The failure this exists to stop is the one that only shows up at sea: a
//! phone that never got Bluetooth permission, a person nobody scanned, a
//! notification channel that was never allowed. Every one of those is fixable
//! in ten seconds on the pier and impossible to fix from the middle of the
//! ocean, so the app has to ask *before* it matters, in a form people will
//! actually finish.
//!
//! That shape is a checklist, not a wizard. Items check themselves off from
//! state the app already has, nothing blocks the rest of the app, and walking
//! away and coming back loses no progress. The policy below is what makes the
//! two shells agree on what "done" and "ready" mean:
//!
//! * **Order is load-bearing, not cosmetic.** The Shore Pass sits first
//!   because a friend code carries the pass's delivery details -- trading
//!   codes before the pass is saved mints codes that are stale the moment they
//!   are scanned. Adding family comes before the permission sweep because a
//!   person with nobody to message has no reason to grant anything.
//! * **Optional means optional.** A family that bought no Shore Pass and never
//!   made a backup is still ready to sail. Only the three required items gate
//!   [`CoreSailChecklistReport::ready`], so the checklist can never tell
//!   someone who is finished that they are not.
//! * **Applicability is decided here, once.** The battery-optimization
//!   exemption exists on Android and has no counterpart on iOS. Rather than
//!   each shell filtering its own sub-rows, iOS passes `None` and this module
//!   returns a permission list with that row absent -- so a grant that cannot
//!   exist can never hold an item open.
//!
//! Nothing here produces user-facing text. The items are enums; the copy for
//! each lives in `strings.xml` and `Localizable.xcstrings`, where the
//! localization gate can see it. Dismissing the home-screen card is
//! presentation state and stays in the shells: it says nothing about whether
//! the family is set up.

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

/// Every fact the checklist consumes, gathered by the shell.
///
/// All of it is state the app already holds for other reasons -- this screen
/// asks no questions, stores no sail date, and collects nothing new. Each
/// field is a settled fact rather than a live radio state on purpose: a
/// checklist item that unticks itself when Bluetooth blinks would train people
/// to ignore it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreSailChecklistInput {
    /// Saved contacts on this device. Matches `BackupInventory.contact_count`.
    pub contact_count: u64,
    /// A Shore Pass is saved on this device (the shell's own stored
    /// [`crate::RelaySetup`] exists). Whether the pass is *reachable* right
    /// now is deliberately not asked: a pass saved in a cabin with no internet
    /// is still set up.
    pub shore_pass_configured: bool,
    /// The platform's permission to use Bluetooth for nearby delivery.
    /// Android: the nearby-devices (`BLUETOOTH_CONNECT`/`BLUETOOTH_SCAN`)
    /// grant. iOS: Bluetooth authorization.
    pub bluetooth_permission: bool,
    /// Permission to post notifications, without which an arriving message is
    /// silent.
    pub notifications_permission: bool,
    /// Android: this app is exempt from battery optimization, so it keeps
    /// carrying messages with the screen off. iOS: `None` -- the setting has
    /// no counterpart there, and an absent grant must never hold the
    /// permissions item open. `Some(false)` is a real, blocking "not granted".
    pub battery_optimization_exempt: Option<bool>,
    /// Any message has ever been delivered on this device over a nearby
    /// transport -- Bluetooth or local Wi-Fi. Internet delivery does not
    /// count: the whole point of the step is proving the phone works when the
    /// internet does not. "Ever", not "recently": a proof that has already
    /// been given is not withdrawn by a quiet afternoon.
    pub offline_delivery_seen: bool,
    /// A local encrypted backup has been made at least once on this device.
    pub backup_created: bool,
}

// ---------------------------------------------------------------------------
// Outputs
// ---------------------------------------------------------------------------

/// The checklist items, in the order they are shown.
///
/// The enum order is the display order, and both are the order the spec fixes;
/// see the module note for why the first two are where they are. Shells map
/// each variant to its own title and subtitle resources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreSailChecklistItemId {
    /// Set up your Shore Pass. Optional -- and first, because friend codes
    /// carry the pass's delivery details.
    ShorePass,
    /// Add your family: scan each other's codes in person, while everyone is
    /// still together. A code swap needs proximity, never signal.
    AddFamily,
    /// Let it run in your pocket: the delivery-critical grants.
    Permissions,
    /// Send a message with no internet, proving nearby delivery works.
    OfflineTest,
    /// Back up your identity. Optional.
    Backup,
}

/// One delivery-critical grant, as its own row.
///
/// Each is a separate row because each opens a different system screen, and a
/// single lumped "permissions" button that reopens the same settings page four
/// times is how people end up granting three of four and believing they are
/// done.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreSailPermission {
    /// Android nearby devices, iOS Bluetooth authorization.
    Bluetooth,
    /// Posting notifications.
    Notifications,
    /// Android only: exemption from battery optimization.
    BatteryOptimization,
}

/// A grant this platform actually has, and whether it is held.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreSailPermissionRow {
    pub permission: CoreSailPermission,
    pub granted: bool,
}

/// One checklist row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreSailChecklistItem {
    pub id: CoreSailChecklistItemId,
    /// Required items gate [`CoreSailChecklistReport::ready`]; optional ones
    /// never do.
    pub required: bool,
    pub done: bool,
}

/// The whole answer: the rows in order, the grants that apply here, and the
/// counts the entry-point card reads.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreSailChecklistReport {
    /// Every item, in display order. Always the full list -- an item is never
    /// dropped, only ticked.
    pub items: Vec<CoreSailChecklistItem>,
    /// The sub-rows of [`CoreSailChecklistItemId::Permissions`], in display
    /// order, filtered to the grants this platform has. Empty rows are never
    /// returned, so a shell can render this list without knowing which
    /// platform it is on.
    pub permissions: Vec<CoreSailPermissionRow>,
    /// Every required item is done. Optional items are not consulted.
    pub ready: bool,
    /// Items done, counting optional ones. With `total_count`, this is the
    /// "N of M done" the home-screen card shows; the card disappears at
    /// `ready`, so nobody is left staring at a total they never intend to
    /// finish.
    pub done_count: u32,
    /// Every item, counting optional ones.
    pub total_count: u32,
    /// Required items done -- the numerator of the progress that actually
    /// gates sailing.
    pub required_done_count: u32,
    /// Required items in total.
    pub required_total_count: u32,
}

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

/// The grants that apply on this device, in display order.
///
/// Bluetooth leads because it is the one grant without which nothing at all
/// moves nearby; battery optimization is last because it is the only one that
/// can be absent entirely.
fn permission_rows(input: &CoreSailChecklistInput) -> Vec<CoreSailPermissionRow> {
    let mut rows = vec![
        CoreSailPermissionRow {
            permission: CoreSailPermission::Bluetooth,
            granted: input.bluetooth_permission,
        },
        CoreSailPermissionRow {
            permission: CoreSailPermission::Notifications,
            granted: input.notifications_permission,
        },
    ];
    if let Some(exempt) = input.battery_optimization_exempt {
        rows.push(CoreSailPermissionRow {
            permission: CoreSailPermission::BatteryOptimization,
            granted: exempt,
        });
    }
    rows
}

/// Turn the facts into the checklist.
///
/// The rules, in full:
///
/// 1. The five items are always all present, always in the order of
///    [`CoreSailChecklistItemId`].
/// 2. `ShorePass` is done when a pass is saved; `AddFamily` when at least one
///    contact exists; `Permissions` when every grant *this platform has* is
///    held; `OfflineTest` when a nearby delivery has ever happened; `Backup`
///    when a local backup has ever been made.
/// 3. `ShorePass` and `Backup` are optional. The other three are required.
/// 4. `ready` is every required item done -- which is exactly `AddFamily`,
///    `Permissions` and `OfflineTest`. No optional item can hold it back, and
///    no optional item can grant it.
#[uniffi::export]
pub fn core_sail_checklist(input: CoreSailChecklistInput) -> CoreSailChecklistReport {
    let permissions = permission_rows(&input);
    // Every grant that exists here has to be held. A platform with no battery
    // setting is not thereby less ready.
    let permissions_done = permissions.iter().all(|row| row.granted);

    let items = vec![
        CoreSailChecklistItem {
            id: CoreSailChecklistItemId::ShorePass,
            required: false,
            done: input.shore_pass_configured,
        },
        CoreSailChecklistItem {
            id: CoreSailChecklistItemId::AddFamily,
            required: true,
            done: input.contact_count >= 1,
        },
        CoreSailChecklistItem {
            id: CoreSailChecklistItemId::Permissions,
            required: true,
            done: permissions_done,
        },
        CoreSailChecklistItem {
            id: CoreSailChecklistItemId::OfflineTest,
            required: true,
            done: input.offline_delivery_seen,
        },
        CoreSailChecklistItem {
            id: CoreSailChecklistItemId::Backup,
            required: false,
            done: input.backup_created,
        },
    ];

    let total_count = items.len() as u32;
    let done_count = items.iter().filter(|item| item.done).count() as u32;
    let required_total_count = items.iter().filter(|item| item.required).count() as u32;
    let required_done_count = items
        .iter()
        .filter(|item| item.required && item.done)
        .count() as u32;

    CoreSailChecklistReport {
        items,
        permissions,
        ready: required_done_count == required_total_count,
        done_count,
        total_count,
        required_done_count,
        required_total_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing done yet, on an Android-shaped device.
    fn fresh() -> CoreSailChecklistInput {
        CoreSailChecklistInput {
            contact_count: 0,
            shore_pass_configured: false,
            bluetooth_permission: false,
            notifications_permission: false,
            battery_optimization_exempt: Some(false),
            offline_delivery_seen: false,
            backup_created: false,
        }
    }

    /// Everything done, on an Android-shaped device.
    fn finished() -> CoreSailChecklistInput {
        CoreSailChecklistInput {
            contact_count: 3,
            shore_pass_configured: true,
            bluetooth_permission: true,
            notifications_permission: true,
            battery_optimization_exempt: Some(true),
            offline_delivery_seen: true,
            backup_created: true,
        }
    }

    /// The three required items done and nothing else, on Android.
    fn required_only() -> CoreSailChecklistInput {
        CoreSailChecklistInput {
            contact_count: 1,
            bluetooth_permission: true,
            notifications_permission: true,
            battery_optimization_exempt: Some(true),
            offline_delivery_seen: true,
            ..fresh()
        }
    }

    fn item(
        report: &CoreSailChecklistReport,
        id: CoreSailChecklistItemId,
    ) -> CoreSailChecklistItem {
        *report
            .items
            .iter()
            .find(|item| item.id == id)
            .expect("every item is always present")
    }

    #[test]
    fn item_order_is_fixed() {
        // The order is the product decision: the pass has to be saved before
        // codes are traded, or the codes carry stale delivery details.
        let report = core_sail_checklist(fresh());
        let ids: Vec<CoreSailChecklistItemId> = report.items.iter().map(|item| item.id).collect();
        assert_eq!(
            ids,
            vec![
                CoreSailChecklistItemId::ShorePass,
                CoreSailChecklistItemId::AddFamily,
                CoreSailChecklistItemId::Permissions,
                CoreSailChecklistItemId::OfflineTest,
                CoreSailChecklistItemId::Backup,
            ]
        );
    }

    #[test]
    fn every_item_is_always_listed() {
        // Items tick; they never disappear. A person who finished a step still
        // gets to see that they finished it.
        for input in [fresh(), required_only(), finished()] {
            assert_eq!(core_sail_checklist(input).items.len(), 5);
        }
    }

    #[test]
    fn shore_pass_and_backup_are_optional_the_rest_required() {
        let report = core_sail_checklist(fresh());
        assert!(!item(&report, CoreSailChecklistItemId::ShorePass).required);
        assert!(item(&report, CoreSailChecklistItemId::AddFamily).required);
        assert!(item(&report, CoreSailChecklistItemId::Permissions).required);
        assert!(item(&report, CoreSailChecklistItemId::OfflineTest).required);
        assert!(!item(&report, CoreSailChecklistItemId::Backup).required);
        assert_eq!(report.required_total_count, 3);
        assert_eq!(report.total_count, 5);
    }

    #[test]
    fn nothing_is_done_on_a_fresh_device() {
        let report = core_sail_checklist(fresh());
        assert!(report.items.iter().all(|item| !item.done));
        assert_eq!(report.done_count, 0);
        assert_eq!(report.required_done_count, 0);
        assert!(!report.ready);
    }

    #[test]
    fn each_item_is_done_only_by_its_own_fact() {
        // One fact at a time: no item may be ticked by a neighbour's state.
        let cases: Vec<(CoreSailChecklistInput, CoreSailChecklistItemId)> = vec![
            (
                CoreSailChecklistInput {
                    shore_pass_configured: true,
                    ..fresh()
                },
                CoreSailChecklistItemId::ShorePass,
            ),
            (
                CoreSailChecklistInput {
                    contact_count: 1,
                    ..fresh()
                },
                CoreSailChecklistItemId::AddFamily,
            ),
            (
                CoreSailChecklistInput {
                    bluetooth_permission: true,
                    notifications_permission: true,
                    battery_optimization_exempt: Some(true),
                    ..fresh()
                },
                CoreSailChecklistItemId::Permissions,
            ),
            (
                CoreSailChecklistInput {
                    offline_delivery_seen: true,
                    ..fresh()
                },
                CoreSailChecklistItemId::OfflineTest,
            ),
            (
                CoreSailChecklistInput {
                    backup_created: true,
                    ..fresh()
                },
                CoreSailChecklistItemId::Backup,
            ),
        ];
        for (input, expected) in cases {
            let report = core_sail_checklist(input);
            for row in &report.items {
                assert_eq!(
                    row.done,
                    row.id == expected,
                    "{:?} should be the only item done",
                    expected
                );
            }
            assert_eq!(report.done_count, 1);
        }
    }

    #[test]
    fn one_contact_is_enough_family() {
        // The boundary: zero is not a family, one is.
        assert!(
            !item(
                &core_sail_checklist(CoreSailChecklistInput {
                    contact_count: 0,
                    ..fresh()
                }),
                CoreSailChecklistItemId::AddFamily
            )
            .done
        );
        assert!(
            item(
                &core_sail_checklist(CoreSailChecklistInput {
                    contact_count: 1,
                    ..fresh()
                }),
                CoreSailChecklistItemId::AddFamily
            )
            .done
        );
    }

    #[test]
    fn permissions_need_every_grant_this_platform_has() {
        // Any one grant missing holds the item open -- three of four granted
        // is the exact state people mistake for finished.
        let all_but_one = [
            CoreSailChecklistInput {
                bluetooth_permission: false,
                notifications_permission: true,
                battery_optimization_exempt: Some(true),
                ..fresh()
            },
            CoreSailChecklistInput {
                bluetooth_permission: true,
                notifications_permission: false,
                battery_optimization_exempt: Some(true),
                ..fresh()
            },
            CoreSailChecklistInput {
                bluetooth_permission: true,
                notifications_permission: true,
                battery_optimization_exempt: Some(false),
                ..fresh()
            },
        ];
        for input in all_but_one {
            let report = core_sail_checklist(input);
            assert!(!item(&report, CoreSailChecklistItemId::Permissions).done);
            assert!(!report.ready);
        }
    }

    #[test]
    fn android_shows_three_permission_rows_carrying_their_own_state() {
        let report = core_sail_checklist(CoreSailChecklistInput {
            bluetooth_permission: true,
            notifications_permission: false,
            battery_optimization_exempt: Some(false),
            ..fresh()
        });
        assert_eq!(
            report.permissions,
            vec![
                CoreSailPermissionRow {
                    permission: CoreSailPermission::Bluetooth,
                    granted: true,
                },
                CoreSailPermissionRow {
                    permission: CoreSailPermission::Notifications,
                    granted: false,
                },
                CoreSailPermissionRow {
                    permission: CoreSailPermission::BatteryOptimization,
                    granted: false,
                },
            ]
        );
    }

    #[test]
    fn ios_drops_the_battery_row_and_is_not_held_open_by_it() {
        // The grant does not exist on iOS, so it must neither be offered nor
        // counted against the item.
        let report = core_sail_checklist(CoreSailChecklistInput {
            bluetooth_permission: true,
            notifications_permission: true,
            battery_optimization_exempt: None,
            ..fresh()
        });
        assert_eq!(
            report.permissions,
            vec![
                CoreSailPermissionRow {
                    permission: CoreSailPermission::Bluetooth,
                    granted: true,
                },
                CoreSailPermissionRow {
                    permission: CoreSailPermission::Notifications,
                    granted: true,
                },
            ]
        );
        assert!(item(&report, CoreSailChecklistItemId::Permissions).done);
    }

    #[test]
    fn ios_still_needs_its_own_two_grants() {
        // Dropping the battery row must not soften the grants iOS does have.
        let report = core_sail_checklist(CoreSailChecklistInput {
            bluetooth_permission: true,
            notifications_permission: false,
            battery_optimization_exempt: None,
            ..fresh()
        });
        assert_eq!(report.permissions.len(), 2);
        assert!(!item(&report, CoreSailChecklistItemId::Permissions).done);
    }

    #[test]
    fn an_ios_device_can_be_ready() {
        // The whole iOS-shaped happy path, with no battery grant anywhere.
        let report = core_sail_checklist(CoreSailChecklistInput {
            battery_optimization_exempt: None,
            ..required_only()
        });
        assert!(report.ready);
        assert_eq!(report.required_done_count, 3);
        assert_eq!(report.done_count, 3);
        assert_eq!(report.total_count, 5);
    }

    #[test]
    fn ready_needs_family_permissions_and_the_offline_test() {
        // Drop each required item in turn from an otherwise finished device.
        let missing_family = CoreSailChecklistInput {
            contact_count: 0,
            ..finished()
        };
        let missing_permissions = CoreSailChecklistInput {
            notifications_permission: false,
            ..finished()
        };
        let missing_offline = CoreSailChecklistInput {
            offline_delivery_seen: false,
            ..finished()
        };
        for input in [missing_family, missing_permissions, missing_offline] {
            assert!(!core_sail_checklist(input).ready);
        }
        assert!(core_sail_checklist(finished()).ready);
    }

    #[test]
    fn optional_items_never_gate_ready() {
        // A family that bought no pass and made no backup is set to sail.
        let report = core_sail_checklist(required_only());
        assert!(report.ready);
        assert!(!item(&report, CoreSailChecklistItemId::ShorePass).done);
        assert!(!item(&report, CoreSailChecklistItemId::Backup).done);
    }

    #[test]
    fn optional_items_never_grant_ready_either() {
        // Both optional items done, no required item done: still not ready.
        let report = core_sail_checklist(CoreSailChecklistInput {
            shore_pass_configured: true,
            backup_created: true,
            ..fresh()
        });
        assert!(!report.ready);
        assert_eq!(report.done_count, 2);
        assert_eq!(report.required_done_count, 0);
    }

    #[test]
    fn counts_track_the_items_they_describe() {
        let report = core_sail_checklist(required_only());
        assert_eq!(report.done_count, 3);
        assert_eq!(report.total_count, 5);
        assert_eq!(report.required_done_count, 3);
        assert_eq!(report.required_total_count, 3);

        let report = core_sail_checklist(finished());
        assert_eq!(report.done_count, 5);
        assert_eq!(report.total_count, 5);
        assert_eq!(report.required_done_count, 3);
        assert_eq!(report.required_total_count, 3);
        assert!(report.ready);
    }

    #[test]
    fn counts_always_agree_with_the_item_list() {
        // The card's "N of M" and the rows it links to can never disagree.
        for input in [fresh(), required_only(), finished()] {
            let report = core_sail_checklist(input);
            assert_eq!(
                report.done_count,
                report.items.iter().filter(|item| item.done).count() as u32
            );
            assert_eq!(report.total_count, report.items.len() as u32);
            assert_eq!(
                report.required_done_count,
                report
                    .items
                    .iter()
                    .filter(|item| item.required && item.done)
                    .count() as u32
            );
            assert_eq!(
                report.ready,
                report.items.iter().all(|item| !item.required || item.done)
            );
        }
    }

    #[test]
    fn the_answer_depends_on_nothing_but_the_input() {
        // Pure: same facts, same report, however many times it is asked.
        assert_eq!(
            core_sail_checklist(required_only()),
            core_sail_checklist(required_only())
        );
    }
}
