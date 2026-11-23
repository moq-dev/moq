// Resolve everything, plugins included, from Google's Maven Central mirror:
// Central answers a shared CI runner IP with 429 for 30 minutes or more, and
// retries extend the block. ../../maven-mirror.sh fails when the mirror's
// dev.moq:moq lags Central's.
pluginManagement {
    repositories { maven("https://maven-central.storage-download.googleapis.com/maven2/") }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories { maven("https://maven-central.storage-download.googleapis.com/maven2/") }
}

rootProject.name = "smoke"
