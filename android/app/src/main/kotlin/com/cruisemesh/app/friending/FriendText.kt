package com.cruisemesh.app.friending

private val FriendLinkRegex = Regex("""CMFRIEND1:[A-Za-z0-9_-]+""")

fun extractFriendToken(text: String): String =
    FriendLinkRegex.find(text)?.value ?: text.trim()
