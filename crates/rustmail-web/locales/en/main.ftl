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

## Webmail
nav-mail = Mail
mail-not-configured = Webmail is not set up on this server. An administrator needs to set MAIL_MASTER_PASSWORD for the web server.
mail-compose = Compose
mail-compose-title = New message
mail-reply-title = Reply
mail-forward-title = Forward
mail-folders = Folders
mail-folder = Folder
mail-messages = Messages
mail-message = Message
mail-message-body = Message content
mail-manage-folders = Manage folders
mail-back = Back to mail
mail-select-message = Select a message to read it.
mail-list-empty = This folder is empty.
mail-count = { $count } messages
mail-page = Page { $page }
mail-pages = Pages
mail-newer = Newer
mail-older = Older
mail-unread = unread
mail-starred = Starred
mail-has-attachments = Has attachments
mail-no-subject = (no subject)
mail-actions = Message actions
mail-reply = Reply
mail-reply-all = Reply all
mail-forward = Forward
mail-delete = Delete
mail-spam = Spam
mail-not-spam = Not spam
mail-star = Star
mail-unstar = Remove star
mail-mark-unread = Mark unread
mail-move-to = Move to folder
mail-move = Move
mail-from = From
mail-to = To
mail-cc = Cc
mail-bcc = Bcc
mail-date = Date
mail-subject = Subject
mail-body = Message
mail-attachments = Attachments
mail-attachments-help = Up to 25 MB in total.
mail-to-help = Separate several addresses with commas.
mail-add-cc = Add Cc
mail-add-bcc = Add Bcc
mail-send = Send
mail-cancel = Cancel
mail-images-blocked = Pictures from the internet are hidden to protect your privacy.
mail-load-images = Show pictures
mail-show-text = Show as plain text
mail-show-html = Show formatted version
mail-search-label = Search mail
mail-search-placeholder = Search mail
mail-search-button = Search
mail-search-results = Search results
mail-search-empty = Nothing found.
mail-search-indexing = Your mailbox is still being indexed, so recent or old mail may not appear yet. Try again in a minute.
mail-notice-sent = Your message was sent.
mail-folder-inbox = Inbox
mail-folder-drafts = Drafts
mail-folder-sent = Sent
mail-folder-trash = Trash
mail-folder-junk = Junk
mail-folder-archive = Archive
mail-folders-new = New folder
mail-folders-inside = Inside
mail-folders-top = (top level)
mail-folders-create = Create folder
mail-folders-messages = Messages
mail-folders-rename = Rename
mail-folders-rename-to = New name
mail-folders-confirm-delete = Delete this folder, its subfolders and every message in them?
mail-folders-created = The folder was created.
mail-folders-renamed = The folder was renamed.
mail-folders-deleted = The folder was deleted.
mail-shortcuts = Keyboard shortcuts
mail-key-move = move up and down the list
mail-key-open = open the message
mail-key-compose = new message
mail-key-reply = reply
mail-key-reply-all = reply all
mail-key-forward = forward
mail-key-delete = delete
mail-key-star = star
mail-key-spam = mark as spam
mail-key-search = search
mail-key-inbox = go to the inbox
mail-key-help = show this help
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
error-400-title = Request not understood
error-400-body = Something in the request is not valid. Check what you entered and try again.
error-503-title = Mail is not available
error-503-body = The mail server could not be reached right now. Please try again in a moment.

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

## Administration console (Phase 7)
admin-domains = Domains
admin-users = Users
admin-aliases = Aliases
admin-groups = Groups
admin-quota = Quota
admin-audit = Audit log
admin-import = Import
admin-health = Health
admin-add = Add
admin-remove = Remove
admin-apply = Apply
admin-create = Create
admin-save = Save
admin-delete = Delete
admin-enable = Enable
admin-disable = Disable
admin-enabled = Enabled
admin-disabled = Disabled
admin-settings = Settings
admin-danger-zone = Danger zone
admin-unlimited = Unlimited
admin-never = Never
admin-inherited = Domain default
admin-all-domains = All domains
admin-any-status = Any status
admin-any-role = Any role
admin-role-user = User
admin-role-domain_admin = Domain administrator
admin-role-super_admin = Super administrator
admin-status-active = Active
admin-status-suspended = Suspended
admin-status-disabled = Disabled
admin-notice-created = Created.
admin-notice-saved = Saved.
admin-notice-deleted = Deleted.
admin-notice-status = The status was changed.
admin-notice-unlocked = The account was unlocked.
admin-notice-signedout = All sessions of this user were ended.
admin-total = { $count } entries
admin-filter-search = Search
admin-filter-apply = Filter
admin-type-to-confirm = Type { $value } to confirm
admin-locked-until = locked until { $time }
admin-need-domain = Create a domain first; a user needs one.
admin-col-domain = Domain
admin-col-status = Status
admin-col-users = Users
admin-col-aliases = Aliases
admin-col-groups = Groups
admin-col-used = Used
admin-col-default-quota = Default quota
admin-col-max-mailboxes = Mailbox limit
admin-col-email = Email
admin-col-name = Name
admin-col-role = Role
admin-col-usage = Usage
admin-col-last-login = Last sign-in
admin-col-quota = Quota
admin-col-alias = Alias
admin-col-destination = Destination
admin-col-actions = Actions
admin-col-address = Address
admin-col-members = Members
admin-domains-empty = There are no domains yet.
admin-domain-new = New domain
admin-field-domain-name = Domain name
admin-field-quota-mb = Quota (MB)
admin-field-quota-help = Leave empty to use the domain default. 0 means unlimited.
admin-field-max-mailboxes = Mailbox limit
admin-field-local-part = Name before the @
admin-field-password = Password
admin-field-new-password = New password
admin-field-password-help = Leave empty to generate one. It is shown once.
admin-field-created = Created
admin-field-messages = Messages
admin-field-sessions = Active sessions
admin-field-failed-logins = Failed sign-ins
admin-field-description = Description
admin-confirm-delete-domain = Delete this domain? This cannot be undone.
admin-delete-domain-help = A domain can only be deleted once it has no users. Its aliases and groups are removed.
admin-user-new = New user
admin-users-empty = No users match.
admin-user-created = User created
admin-password-reset = Password changed
admin-secret-once = This password is shown only once. Pass it on securely.
admin-secret-chosen = The password you entered was set.
admin-secret-continue = Continue
admin-reset-password = Set new password
admin-confirm-reset-password = Set a new password? The user's sessions end.
admin-unlock = Unlock account
admin-sign-out-everywhere = Sign out everywhere
admin-confirm-signout = End all sessions of this user?
admin-confirm-delete-user = Delete this user and the mailbox? This cannot be undone.
admin-delete-user-help = Deleting removes the account and all its mail from disk. To keep the mail, set the status to Disabled instead.
admin-aliases-intro = An alias forwards mail for one address to another.
admin-aliases-empty = There are no aliases.
admin-alias-new = New alias
admin-confirm-delete-alias = Delete this alias?
admin-groups-intro = A group with an address delivers mail to all of its members.
admin-groups-empty = There are no groups.
admin-group-new = New group
admin-group-address-help = Optional. Mail to this address goes to every member.
admin-group-no-members = This group has no members.
admin-group-add-member = Add a member (email)
admin-confirm-delete-group = Delete this group?
admin-quota-total-used = Total used
admin-quota-mailboxes = Mailboxes
admin-quota-by-domain = Usage by domain
admin-quota-top = Largest mailboxes
admin-quota-near = Near the limit
admin-quota-near-none = No mailbox is close to its limit.
admin-quota-label-used = Used (MB)
admin-quota-label-limit = Quota assigned (MB)
admin-quota-label-percent = Quota used (%)
admin-quota-unlimited-mailboxes = Without limit
admin-quota-chart-failed = The charts could not be loaded. The tables below show the same figures.
admin-quota-chart-fallback = Chart of mailbox usage. The same figures are in the tables below.
admin-audit-actor = Actor
admin-audit-action = Action
admin-audit-any-action = Any action
admin-audit-outcome = Outcome
admin-audit-any-outcome = Any outcome
admin-audit-success = Success
admin-audit-failure = Failure
admin-audit-from = From
admin-audit-to = To
admin-audit-text = Contains
admin-audit-export = Export CSV
admin-audit-empty = No entries match.
admin-audit-when = Time
admin-audit-target = Target
admin-audit-ip = IP address
admin-audit-details = Details
admin-audit-show = Show
admin-import-intro = Create many users from a CSV file. Check the preview first; nothing is created until you confirm. At most 1000 rows and 1 MB.
admin-import-file = CSV file
admin-import-paste = Or paste the CSV
admin-import-preview = Check
admin-import-preview-title = Preview
admin-import-valid-count = { $count } valid rows
admin-import-problem-count = { $count } with problems
admin-import-line = Line
admin-import-problem = Problem
admin-import-password-given = Given
admin-import-password-generated = Will be generated
admin-import-fix-first = Fix the problems in the file and check it again. Nothing was created.
admin-import-apply = Create { $count } users
admin-import-result-title = Result
admin-import-created-count = { $count } created
admin-import-failed-count = { $count } failed
admin-import-created = Created
admin-import-download = Download passwords (CSV)
admin-import-download-help = The generated passwords are in this file. You can download it once, within 10 minutes. It is not kept after that.
admin-health-intro = Checks run from this server. "Not checked" means the service cannot be probed from here, not that it is down.
admin-health-col-check = Check
admin-health-col-status = Status
admin-health-col-detail = Detail
admin-health-ok = OK
admin-health-warn = Warning
admin-health-down = Down
admin-health-unchecked = Not checked
admin-health-database = Database
admin-health-imap = IMAP (Dovecot)
admin-health-smtp = SMTP (Postfix)
admin-health-redis = Redis
admin-health-clamav = Virus scanner
admin-health-amavis = Amavis
admin-health-queue = Mail queue
admin-health-pipeline = Mail pipeline log
admin-health-disk = Disk space
admin-health-systemd = System services
admin-health-service = Service
