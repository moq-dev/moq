package dev.moq

import kotlinx.serialization.json.Json

/**
 * The [Json] format used by the typed helpers here and in `dev.moq.json`.
 *
 * Lenient about unknown keys so a producer adding a field does not break older
 * subscribers, matching how the catalog treats untyped sections.
 */
val MoqJson: Json = Json { ignoreUnknownKeys = true }
