// Top-level build file. Plugin declarations for subprojects go in app/build.gradle.kts.
// NOTE: AGP 9.0+ ships built-in Kotlin support (a runtime dep on the Kotlin Gradle Plugin),
// so the standalone `org.jetbrains.kotlin.android` plugin is no longer declared — applying it
// is now an error. Kotlin is provided (and version-managed) by AGP itself. See
// https://developer.android.com/build/releases/agp-9-0-0-release-notes#android-gradle-plugin-built-in-kotlin
plugins {
    id("com.android.application") version "9.2.1" apply false
}
