// Entry point of the vendored calendar bundle: FullCalendar with the month, week, day and
// list views, drag-and-drop interaction, and the German locale (English is built in).
import { Calendar } from '@fullcalendar/core';
import deLocale from '@fullcalendar/core/locales/de';
import dayGridPlugin from '@fullcalendar/daygrid';
import timeGridPlugin from '@fullcalendar/timegrid';
import listPlugin from '@fullcalendar/list';
import interactionPlugin from '@fullcalendar/interaction';

window.RustMailCalendar = {
  Calendar,
  plugins: [dayGridPlugin, timeGridPlugin, listPlugin, interactionPlugin],
  locales: [deLocale],
};
