# RustMail Suite: Deutsch.
#
# Hinweis: Diese Übersetzung ist ein erster Entwurf und sollte von einer
# Muttersprachlerin oder einem Muttersprachler geprüft werden.
# Jeder Schlüssel aus en/main.ftl muss hier vorhanden sein (ein Test prüft das).

## Allgemein

app-name = RustMail
skip-to-content = Zum Hauptinhalt springen
nav-dashboard = Übersicht
nav-admin = Administration
nav-toggle = Navigation umschalten
account-menu = Kontomenü
signed-in-as = Angemeldet als { $name }
sign-out = Abmelden
sign-out-everywhere = Von allen Geräten abmelden
cancel = Abbrechen
close = Schließen
loading = Wird geladen…
footer-version = RustMail Suite { $version }

## Design und Sprache

theme-menu = Design
theme-auto = Automatisch
theme-light = Hell
theme-dark = Dunkel
language-menu = Sprache

## Anmeldung

login-title = Anmelden
login-subtitle = Melden Sie sich bei Ihrem Postfach an
login-email = E-Mail-Adresse
login-password = Passwort
login-show-password = Passwort anzeigen
login-hide-password = Passwort verbergen
login-submit = Anmelden
login-submitting = Anmeldung läuft…
login-noscript = Zum Anmelden ist JavaScript erforderlich.
login-error-missing = Geben Sie Ihre E-Mail-Adresse und Ihr Passwort ein.
login-error-invalid = E-Mail-Adresse oder Passwort ist ungültig.
login-error-locked =
    Zu viele fehlgeschlagene Versuche. Versuchen Sie es in { $minutes ->
        [one] 1 Minute
       *[other] { $minutes } Minuten
    } erneut.
login-error-disabled = Dieses Konto ist deaktiviert. Wenden Sie sich an Ihre Administration.
login-error-generic = Etwas ist schiefgelaufen. Bitte versuchen Sie es erneut.

## Übersicht

dashboard-title = Übersicht
dashboard-welcome = Willkommen zurück, { $name }
dashboard-account = Ihr Konto
dashboard-email = E-Mail-Adresse
dashboard-role = Rolle
role-super-admin = Hauptadministrator
role-domain-admin = Domain-Administrator
role-user = Benutzer
dashboard-activity-title = Letzte Anmeldeaktivität
dashboard-activity-intro = Anmeldeversuche für Ihr Konto. Wenn Sie etwas nicht erkennen, melden Sie sich von allen Geräten ab und ändern Sie Ihr Passwort.
dashboard-activity-empty = Noch keine Anmeldeaktivität aufgezeichnet.
dashboard-activity-count =
    { $count ->
        [one] 1 Ereignis
       *[other] { $count } Ereignisse
    }
activity-when = Zeitpunkt
activity-result = Ergebnis
activity-address = Adresse
activity-device = Gerät
activity-success = Angemeldet
activity-failure = Fehlgeschlagen

## Von allen Geräten abmelden

logout-all-title = Von allen Geräten abmelden?
logout-all-body = Dadurch werden alle aktiven Sitzungen Ihres Kontos beendet, auch diese.
logout-all-confirm = Überall abmelden

## Administration

admin-title = Administration
admin-overview = Überblick
admin-stat-domains = Domains
admin-stat-database = Datenbank
admin-stat-version = Serverversion
admin-db-ok = Verbunden ({ $ms } ms)
admin-db-error = Nicht erreichbar
admin-refreshed = Aktualisiert { $time }

## Webmail
nav-mail = E-Mail
mail-not-configured = Webmail ist auf diesem Server nicht eingerichtet. Eine Administratorin oder ein Administrator muss MAIL_MASTER_PASSWORD für den Webserver setzen.
mail-compose = Verfassen
mail-compose-title = Neue Nachricht
mail-reply-title = Antworten
mail-forward-title = Weiterleiten
mail-folders = Ordner
mail-folder = Ordner
mail-messages = Nachrichten
mail-message = Nachricht
mail-message-body = Nachrichteninhalt
mail-manage-folders = Ordner verwalten
mail-back = Zurück zur E-Mail
mail-select-message = Wählen Sie eine Nachricht aus, um sie zu lesen.
mail-list-empty = Dieser Ordner ist leer.
mail-count = { $count } Nachrichten
mail-page = Seite { $page }
mail-pages = Seiten
mail-newer = Neuere
mail-older = Ältere
mail-unread = ungelesen
mail-starred = Markiert
mail-has-attachments = Mit Anhängen
mail-no-subject = (kein Betreff)
mail-actions = Aktionen für die Nachricht
mail-reply = Antworten
mail-reply-all = Allen antworten
mail-forward = Weiterleiten
mail-delete = Löschen
mail-spam = Spam
mail-not-spam = Kein Spam
mail-star = Markieren
mail-unstar = Markierung entfernen
mail-mark-unread = Als ungelesen markieren
mail-move-to = In Ordner verschieben
mail-move = Verschieben
mail-from = Von
mail-to = An
mail-cc = Cc
mail-bcc = Bcc
mail-date = Datum
mail-subject = Betreff
mail-body = Nachricht
mail-attachments = Anhänge
mail-attachments-help = Insgesamt bis zu 25 MB.
mail-to-help = Mehrere Adressen mit Kommas trennen.
mail-add-cc = Cc hinzufügen
mail-add-bcc = Bcc hinzufügen
mail-send = Senden
mail-cancel = Abbrechen
mail-images-blocked = Bilder aus dem Internet werden zum Schutz Ihrer Privatsphäre nicht geladen.
mail-load-images = Bilder anzeigen
mail-show-text = Als reinen Text anzeigen
mail-show-html = Formatierte Ansicht anzeigen
mail-search-label = E-Mails durchsuchen
mail-search-placeholder = E-Mails durchsuchen
mail-search-button = Suchen
mail-search-results = Suchergebnisse
mail-search-empty = Nichts gefunden.
mail-search-indexing = Ihr Postfach wird noch indiziert, daher fehlen neue oder alte Nachrichten möglicherweise. Versuchen Sie es in einer Minute erneut.
mail-notice-sent = Ihre Nachricht wurde gesendet.
mail-folder-inbox = Posteingang
mail-folder-drafts = Entwürfe
mail-folder-sent = Gesendet
mail-folder-trash = Papierkorb
mail-folder-junk = Spam
mail-folder-archive = Archiv
mail-folders-new = Neuer Ordner
mail-folders-inside = Innerhalb von
mail-folders-top = (oberste Ebene)
mail-folders-create = Ordner anlegen
mail-folders-messages = Nachrichten
mail-folders-rename = Umbenennen
mail-folders-rename-to = Neuer Name
mail-folders-confirm-delete = Diesen Ordner, seine Unterordner und alle darin enthaltenen Nachrichten löschen?
mail-folders-created = Der Ordner wurde angelegt.
mail-folders-renamed = Der Ordner wurde umbenannt.
mail-folders-deleted = Der Ordner wurde gelöscht.
mail-shortcuts = Tastenkürzel
mail-key-move = in der Liste auf und ab
mail-key-open = Nachricht öffnen
mail-key-compose = neue Nachricht
mail-key-reply = antworten
mail-key-reply-all = allen antworten
mail-key-forward = weiterleiten
mail-key-delete = löschen
mail-key-star = markieren
mail-key-spam = als Spam markieren
mail-key-search = suchen
mail-key-inbox = zum Posteingang
mail-key-help = diese Hilfe anzeigen
## Quarantäne
admin-quarantine = Quarantäne
quarantine-title = Quarantäne
quarantine-intro = Nachrichten, die von den Viren- und Spamfiltern zurückgehalten wurden. Keine davon wurde zugestellt.
quarantine-not-configured = Die Quarantäne ist für diesen Webserver nicht eingerichtet. Setzen Sie QUARANTINE_DIR in seiner Umgebung.
quarantine-count-virus = Viren zurückgehalten
quarantine-count-spam = Spam zurückgehalten
quarantine-count-other = Sonstiges zurückgehalten
quarantine-filter-status = Status
quarantine-filter-kind = Art
quarantine-filter-search = Absender oder Empfänger
quarantine-filter-apply = Filtern
quarantine-status-all = Jeder Status
quarantine-status-held = Zurückgehalten
quarantine-status-released = Freigegeben
quarantine-status-deleted = Gelöscht
quarantine-kind-all = Alle Arten
quarantine-kind-virus = Virus
quarantine-kind-spam = Spam
quarantine-kind-banned = Verbotene Datei
quarantine-kind-bad-header = Fehlerhafter Header
quarantine-kind-other = Sonstiges
quarantine-col-received = Empfangen
quarantine-col-kind = Art
quarantine-col-reason = Grund
quarantine-col-sender = Von
quarantine-col-recipient = An
quarantine-col-size = Größe
quarantine-col-status = Status
quarantine-view = Ansehen
quarantine-empty = Nichts passt zum Filter.
quarantine-total = { $count } Nachrichten
quarantine-page = Seite { $page }
quarantine-prev = Zurück
quarantine-next = Weiter
quarantine-pagination = Seiten
quarantine-notice-released = Die Nachricht wurde dem Empfänger zugestellt.
quarantine-notice-deleted = Die Nachricht wurde gelöscht.
quarantine-notice-failed = Die Aktion ist fehlgeschlagen. Die Nachricht ist weiterhin in Quarantäne; siehe Serverprotokoll.
quarantine-notice-gone = Diese Nachricht wird nicht mehr zurückgehalten.
quarantine-detail-title = Nachricht in Quarantäne
quarantine-back = Zurück zur Quarantäne
quarantine-headers = Header
quarantine-excerpt = Anfang der Nachricht
quarantine-excerpt-cut = Die Nachricht ist länger; nur der Anfang wird angezeigt.
quarantine-no-excerpt = Diese Nachricht ist kein reiner Text, daher gibt es keine Vorschau.
quarantine-file-missing = Die Nachrichtendatei konnte nicht aus dem Quarantäneverzeichnis gelesen werden.
quarantine-release = An Empfänger freigeben
quarantine-delete = Löschen
quarantine-confirm-release = Diese Nachricht dem Empfänger zustellen? Virus- und Spamprüfung werden übersprungen.
quarantine-confirm-release-virus = Diese Nachricht enthält einen Virus. Trotzdem dem Empfänger zustellen?
quarantine-confirm-delete = Diese Nachricht endgültig löschen?
quarantine-resolved = Erledigt { $when }
## Fehler

error-back-home = Zurück zur Übersicht
error-404-title = Seite nicht gefunden
error-404-body = Die gesuchte Seite existiert nicht.
error-403-title = Zugriff verweigert
error-403-body = Sie haben keine Berechtigung, diese Seite anzuzeigen.
error-500-title = Etwas ist schiefgelaufen
error-500-body = Ein unerwarteter Fehler ist aufgetreten. Bitte versuchen Sie es später erneut.
error-400-title = Anfrage nicht verständlich
error-400-body = Etwas in der Anfrage ist ungültig. Prüfen Sie Ihre Eingabe und versuchen Sie es erneut.
error-503-title = E-Mail ist nicht verfügbar
error-503-body = Der Mailserver ist gerade nicht erreichbar. Bitte versuchen Sie es gleich noch einmal.

## Benachrichtigungen (Toasts)

toast-network = Netzwerkfehler. Prüfen Sie Ihre Verbindung und versuchen Sie es erneut.
toast-server = Der Server hat einen Fehler gemeldet. Bitte versuchen Sie es erneut.
toast-csrf = Ihre Seite ist veraltet. Laden Sie sie neu und versuchen Sie es erneut.
toast-rate-limited = Zu viele Versuche. Bitte warten Sie einen Moment und versuchen Sie es erneut.
toast-forbidden = Dazu haben Sie keine Berechtigung.

## Sitzung wiederherstellen

renew-title = Sitzung wird wiederhergestellt
renew-body = Einen Moment…
renew-failed = Ihre Sitzung ist abgelaufen. Sie werden zur Anmeldung weitergeleitet…

## Verwaltungskonsole (Phase 7)
admin-domains = Domänen
admin-users = Benutzer
admin-aliases = Aliasse
admin-groups = Gruppen
admin-quota = Kontingent
admin-audit = Audit-Protokoll
admin-import = Import
admin-health = Systemzustand
admin-add = Hinzufügen
admin-remove = Entfernen
admin-apply = Übernehmen
admin-create = Anlegen
admin-save = Speichern
admin-delete = Löschen
admin-enable = Aktivieren
admin-disable = Deaktivieren
admin-enabled = Aktiv
admin-disabled = Inaktiv
admin-settings = Einstellungen
admin-danger-zone = Gefahrenbereich
admin-unlimited = Unbegrenzt
admin-never = Nie
admin-inherited = Domänenstandard
admin-all-domains = Alle Domänen
admin-any-status = Beliebiger Status
admin-any-role = Beliebige Rolle
admin-role-user = Benutzer
admin-role-domain_admin = Domänenadministrator
admin-role-super_admin = Superadministrator
admin-status-active = Aktiv
admin-status-suspended = Gesperrt
admin-status-disabled = Deaktiviert
admin-notice-created = Angelegt.
admin-notice-saved = Gespeichert.
admin-notice-deleted = Gelöscht.
admin-notice-status = Der Status wurde geändert.
admin-notice-unlocked = Das Konto wurde entsperrt.
admin-notice-signedout = Alle Sitzungen dieses Benutzers wurden beendet.
admin-total = { $count } Einträge
admin-filter-search = Suche
admin-filter-apply = Filtern
admin-type-to-confirm = Zur Bestätigung { $value } eingeben
admin-locked-until = gesperrt bis { $time }
admin-need-domain = Legen Sie zuerst eine Domäne an; ein Benutzer braucht eine.
admin-col-domain = Domäne
admin-col-status = Status
admin-col-users = Benutzer
admin-col-aliases = Aliasse
admin-col-groups = Gruppen
admin-col-used = Belegt
admin-col-default-quota = Standardkontingent
admin-col-max-mailboxes = Postfachlimit
admin-col-email = E-Mail
admin-col-name = Name
admin-col-role = Rolle
admin-col-usage = Nutzung
admin-col-last-login = Letzte Anmeldung
admin-col-quota = Kontingent
admin-col-alias = Alias
admin-col-destination = Ziel
admin-col-actions = Aktionen
admin-col-address = Adresse
admin-col-members = Mitglieder
admin-domains-empty = Es gibt noch keine Domänen.
admin-domain-new = Neue Domäne
admin-field-domain-name = Domänenname
admin-field-quota-mb = Kontingent (MB)
admin-field-quota-help = Leer lassen für den Domänenstandard. 0 bedeutet unbegrenzt.
admin-field-max-mailboxes = Postfachlimit
admin-field-local-part = Name vor dem @
admin-field-password = Passwort
admin-field-new-password = Neues Passwort
admin-field-password-help = Leer lassen, um eines zu erzeugen. Es wird einmalig angezeigt.
admin-field-created = Angelegt
admin-field-messages = Nachrichten
admin-field-sessions = Aktive Sitzungen
admin-field-failed-logins = Fehlgeschlagene Anmeldungen
admin-field-description = Beschreibung
admin-confirm-delete-domain = Diese Domäne löschen? Das kann nicht rückgängig gemacht werden.
admin-delete-domain-help = Eine Domäne kann nur gelöscht werden, wenn sie keine Benutzer mehr hat. Aliasse und Gruppen werden mitgelöscht.
admin-user-new = Neuer Benutzer
admin-users-empty = Keine passenden Benutzer.
admin-user-created = Benutzer angelegt
admin-password-reset = Passwort geändert
admin-secret-once = Dieses Passwort wird nur einmal angezeigt. Geben Sie es sicher weiter.
admin-secret-chosen = Das eingegebene Passwort wurde gesetzt.
admin-secret-continue = Weiter
admin-reset-password = Neues Passwort setzen
admin-confirm-reset-password = Neues Passwort setzen? Die Sitzungen des Benutzers werden beendet.
admin-unlock = Konto entsperren
admin-sign-out-everywhere = Überall abmelden
admin-confirm-signout = Alle Sitzungen dieses Benutzers beenden?
admin-confirm-delete-user = Diesen Benutzer samt Postfach löschen? Das kann nicht rückgängig gemacht werden.
admin-delete-user-help = Beim Löschen werden das Konto und alle E-Mails vom Datenträger entfernt. Um die E-Mails zu behalten, setzen Sie den Status stattdessen auf Deaktiviert.
admin-aliases-intro = Ein Alias leitet E-Mails für eine Adresse an eine andere weiter.
admin-aliases-empty = Es gibt keine Aliasse.
admin-alias-new = Neuer Alias
admin-confirm-delete-alias = Diesen Alias löschen?
admin-groups-intro = Eine Gruppe mit Adresse stellt E-Mails an alle Mitglieder zu.
admin-groups-empty = Es gibt keine Gruppen.
admin-group-new = Neue Gruppe
admin-group-address-help = Optional. E-Mails an diese Adresse gehen an alle Mitglieder.
admin-group-no-members = Diese Gruppe hat keine Mitglieder.
admin-group-add-member = Mitglied hinzufügen (E-Mail)
admin-confirm-delete-group = Diese Gruppe löschen?
admin-quota-total-used = Insgesamt belegt
admin-quota-mailboxes = Postfächer
admin-quota-by-domain = Belegung nach Domäne
admin-quota-top = Größte Postfächer
admin-quota-near = Nahe am Limit
admin-quota-near-none = Kein Postfach ist nahe am Limit.
admin-quota-label-used = Belegt (MB)
admin-quota-label-limit = Vergebenes Kontingent (MB)
admin-quota-label-percent = Kontingent belegt (%)
admin-quota-unlimited-mailboxes = Ohne Limit
admin-quota-chart-failed = Die Diagramme konnten nicht geladen werden. Die Tabellen unten zeigen dieselben Werte.
admin-quota-chart-fallback = Diagramm der Postfachbelegung. Die Werte stehen auch in den Tabellen unten.
admin-audit-actor = Akteur
admin-audit-action = Aktion
admin-audit-any-action = Beliebige Aktion
admin-audit-outcome = Ergebnis
admin-audit-any-outcome = Beliebiges Ergebnis
admin-audit-success = Erfolg
admin-audit-failure = Fehlschlag
admin-audit-from = Von
admin-audit-to = Bis
admin-audit-text = Enthält
admin-audit-export = CSV exportieren
admin-audit-empty = Keine passenden Einträge.
admin-audit-when = Zeit
admin-audit-target = Ziel
admin-audit-ip = IP-Adresse
admin-audit-details = Details
admin-audit-show = Anzeigen
admin-import-intro = Legen Sie viele Benutzer aus einer CSV-Datei an. Prüfen Sie zuerst die Vorschau; erst nach Ihrer Bestätigung wird etwas angelegt. Höchstens 1000 Zeilen und 1 MB.
admin-import-file = CSV-Datei
admin-import-paste = Oder CSV einfügen
admin-import-preview = Prüfen
admin-import-preview-title = Vorschau
admin-import-valid-count = { $count } gültige Zeilen
admin-import-problem-count = { $count } mit Problemen
admin-import-line = Zeile
admin-import-problem = Problem
admin-import-password-given = Vorgegeben
admin-import-password-generated = Wird erzeugt
admin-import-fix-first = Beheben Sie die Probleme in der Datei und prüfen Sie erneut. Es wurde nichts angelegt.
admin-import-apply = { $count } Benutzer anlegen
admin-import-result-title = Ergebnis
admin-import-created-count = { $count } angelegt
admin-import-failed-count = { $count } fehlgeschlagen
admin-import-created = Angelegt
admin-import-download = Passwörter herunterladen (CSV)
admin-import-download-help = Die erzeugten Passwörter stehen in dieser Datei. Sie können sie einmal innerhalb von 10 Minuten herunterladen. Danach wird sie nicht aufbewahrt.
admin-health-intro = Die Prüfungen laufen von diesem Server aus. „Nicht geprüft“ heißt, dass der Dienst von hier aus nicht prüfbar ist, nicht dass er ausgefallen ist.
admin-health-col-check = Prüfung
admin-health-col-status = Status
admin-health-col-detail = Detail
admin-health-ok = OK
admin-health-warn = Warnung
admin-health-down = Ausgefallen
admin-health-unchecked = Nicht geprüft
admin-health-database = Datenbank
admin-health-imap = IMAP (Dovecot)
admin-health-smtp = SMTP (Postfix)
admin-health-redis = Redis
admin-health-clamav = Virenscanner
admin-health-amavis = Amavis
admin-health-queue = Mail-Warteschlange
admin-health-pipeline = Mail-Pipeline-Protokoll
admin-health-disk = Speicherplatz
admin-health-systemd = Systemdienste
admin-health-service = Dienst
