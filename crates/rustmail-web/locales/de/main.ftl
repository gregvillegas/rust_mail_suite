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
