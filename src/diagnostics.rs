//! User-facing diagnostics accept typed categories and return static text only.
//! Never interpolate transcripts, clipboard contents, native errors, paths or URLs.
use crate::inserter::InsertError;
use crate::updater::UpdateFailure;

pub(crate) const fn insertion_message(error: InsertError) -> &'static str {
    match error {
        InsertError::EmptyText => "Нет текста для вставки",
        InsertError::SecureField => "В защищённое поле вставлять нельзя. Выберите обычное поле",
        InsertError::Accessibility(_) => {
            "Не удалось проверить поле ввода. Выберите поле и проверьте универсальный доступ"
        }
        InsertError::PasteboardSnapshot => "Не удалось сохранить буфер обмена. Повторите диктовку",
        InsertError::PasteboardWrite => "Не удалось записать текст в буфер обмена",
        InsertError::PasteboardChanged => "Буфер обмена изменился. Повторите диктовку",
        InsertError::PasteboardRestore => "Не удалось восстановить буфер обмена",
        InsertError::EventSource | InsertError::KeyboardEvent => {
            "Не удалось отправить команду вставки. Повторите диктовку"
        }
    }
}

pub(crate) const fn update_message(failure: UpdateFailure) -> &'static str {
    match failure {
        UpdateFailure::WorkerStopped => "Обработчик обновлений остановился. Повторите попытку",
        UpdateFailure::Network => "Ошибка сети. Проверьте подключение",
        UpdateFailure::UntrustedManifest => "Не удалось проверить подлинность обновления",
        UpdateFailure::Storage => "Не удалось прочитать или сохранить файлы",
        UpdateFailure::DigestMismatch => "Контрольная сумма обновления не совпала",
        UpdateFailure::InvalidArtifactSize => "Недопустимый размер файла обновления",
        UpdateFailure::ContentLengthMismatch => "Размер загрузки не совпал с подписанным описанием",
        UpdateFailure::BodySizeMismatch => "Размер загруженного файла не совпал с описанием",
        UpdateFailure::ManifestTooLarge => "Описание обновления превышает допустимый размер",
        UpdateFailure::HttpStatus => "Неверный ответ сервера обновлений",
        UpdateFailure::InsecureTransport => "Адрес или соединение обновления не прошли проверку",
        UpdateFailure::QuarantineMissing => {
            "Не удалось безопасно подготовить обновление. Повторите загрузку"
        }
        UpdateFailure::WrongThread => "Обновление остановлено из-за внутренней ошибки",
        UpdateFailure::OpenDmg => "Не удалось открыть образ обновления",
    }
}
