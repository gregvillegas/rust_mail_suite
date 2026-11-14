# RustMail Suite: English (source language).
#
# Rules for translators and developers:
#  - Every key here must exist in every other locale (a test enforces it).
#  - Keys used in templates are checked against this file by a test.
#  - Use Fluent selectors for plurals; never build plurals in code.

## Common

app-name = RustMail
skip-to-content = Skip to main content
nav-dashboard = Dashboard
nav-admin = Administration
nav-toggle = Toggle navigation
account-menu = Account menu
signed-in-as = Signed in as { $name }
sign-out = Sign out
sign-out-everywhere = Sign out of all devices
cancel = Cancel
close = Close
loading = Loading…
footer-version = RustMail Suite { $version }

## Theme and language

theme-menu = Theme
theme-auto = Auto
theme-light = Light
theme-dark = Dark
language-menu = Language

## Sign in

login-title = Sign in
login-subtitle = Sign in to your mailbox
login-email = Email address
login-password = Password
login-show-password = Show password
login-hide-password = Hide password
login-submit = Sign in
login-submitting = Signing in…
login-noscript = JavaScript is required to sign in.
login-error-missing = Enter your email address and password.
login-error-invalid = Invalid email or password.
login-error-locked =
    Too many failed attempts. Try again in { $minutes ->
        [one] 1 minute
       *[other] { $minutes } minutes
    }.
login-error-disabled = This account is disabled. Contact your administrator.
login-error-generic = Something went wrong. Please try again.

## Dashboard

dashboard-title = Dashboard
dashboard-welcome = Welcome back, { $name }
dashboard-account = Your account
dashboard-email = Email address
dashboard-role = Role
role-super-admin = Super administrator
role-domain-admin = Domain administrator
role-user = User
dashboard-activity-title = Recent sign-in activity
dashboard-activity-intro = Sign-in attempts on your account. If you see something you do not recognise, sign out of all devices and change your password.
dashboard-activity-empty = No sign-in activity recorded yet.
dashboard-activity-count =
    { $count ->
        [one] 1 event
       *[other] { $count } events
    }
activity-when = When
activity-result = Result
activity-address = Address
activity-device = Device
activity-success = Signed in
activity-failure = Failed

## Sign out of all devices

logout-all-title = Sign out of all devices?
logout-all-body = This ends every active session for your account, including this one.
logout-all-confirm = Sign out everywhere

## Administration

admin-title = Administration
admin-overview = Overview
admin-stat-domains = Domains
admin-stat-database = Database
admin-stat-version = Server version
admin-db-ok = Connected ({ $ms } ms)
admin-db-error = Unavailable
admin-refreshed = Updated { $time }

## Quarantine
admin-quarantine = Quarantine
quarantine-title = Quarantine
quarantine-intro = Messages held back by the virus and spam filters. None of these has been delivered.
quarantine-not-configured = Quarantine is not set up for this web server. Set QUARANTINE_DIR in its environment.
quarantine-count-virus = Viruses held
quarantine-count-spam = Spam held
quarantine-count-other = Other held
quarantine-filter-status = Status
quarantine-filter-kind = Type
quarantine-filter-search = Sender or recipient
quarantine-filter-apply = Filter
quarantine-status-all = Any status
quarantine-status-held = Held
quarantine-status-released = Released
quarantine-status-deleted = Deleted
quarantine-kind-all = All types
quarantine-kind-virus = Virus
quarantine-kind-spam = Spam
quarantine-kind-banned = Banned file
quarantine-kind-bad-header = Bad header
quarantine-kind-other = Other
quarantine-col-received = Received
quarantine-col-kind = Type
quarantine-col-reason = Reason
quarantine-col-sender = From
quarantine-col-recipient = To
quarantine-col-size = Size
quarantine-col-status = Status
quarantine-view = View
quarantine-empty = Nothing matches.
quarantine-total = { $count } messages
quarantine-page = Page { $page }
quarantine-prev = Previous
quarantine-next = Next
quarantine-pagination = Pages
quarantine-notice-released = The message was delivered to the recipient.
quarantine-notice-deleted = The message was deleted.
quarantine-notice-failed = The action failed. The message is still in quarantine; see the server log.
quarantine-notice-gone = That message is no longer held.
quarantine-detail-title = Quarantined message
quarantine-back = Back to quarantine
quarantine-headers = Headers
quarantine-excerpt = Start of the message
quarantine-excerpt-cut = The message continues; only the start is shown.
quarantine-no-excerpt = This message is not plain text, so no preview is shown.
quarantine-file-missing = The message file could not be read from the quarantine directory.
quarantine-release = Release to recipient
quarantine-delete = Delete
quarantine-confirm-release = Deliver this message to the recipient? It skips the virus and spam checks.
quarantine-confirm-release-virus = This message contains a virus. Deliver it to the recipient anyway?
quarantine-confirm-delete = Delete this message permanently?
quarantine-resolved = Resolved { $when }
## Errors

error-back-home = Back to dashboard
error-404-title = Page not found
error-404-body = The page you are looking for does not exist.
error-403-title = Access denied
error-403-body = You do not have permission to view this page.
error-500-title = Something went wrong
error-500-body = An unexpected error occurred. Please try again later.

## Notifications (shown as toasts)

toast-network = Network error. Check your connection and try again.
toast-server = The server reported an error. Please try again.
toast-csrf = Your page is out of date. Reload it and try again.
toast-rate-limited = Too many attempts. Please wait a moment and try again.
toast-forbidden = You do not have permission to do that.

## Session restore

renew-title = Restoring your session
renew-body = One moment…
renew-failed = Your session has expired. Taking you to the sign-in page…
