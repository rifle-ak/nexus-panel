export type User = {
  id: string
  email: string
  username: string
  name?: string
  avatar?: string
  bio?: string
  role: 'USER' | 'MODERATOR' | 'ADMIN'
  createdAt: Date
  updatedAt: Date
}

export type Product = {
  id: string
  name: string
  description: string
  price: number
  image?: string
  category: string
  stock: number
  active: boolean
}

export type Order = {
  id: string
  userId: string
  status: 'PENDING' | 'PROCESSING' | 'COMPLETED' | 'CANCELLED' | 'REFUNDED'
  total: number
  createdAt: Date
  orderItems: OrderItem[]
}

export type OrderItem = {
  id: string
  productId: string
  quantity: number
  price: number
  product: Product
}

export type ForumPost = {
  id: string
  title: string
  content: string
  categoryId: string
  authorId: string
  pinned: boolean
  locked: boolean
  views: number
  createdAt: Date
  updatedAt: Date
  author: User
  category: ForumCategory
  replies: ForumReply[]
}

export type ForumCategory = {
  id: string
  name: string
  description?: string
  slug: string
  order: number
}

export type ForumReply = {
  id: string
  content: string
  postId: string
  authorId: string
  createdAt: Date
  author: User
}

export type Post = {
  id: string
  title: string
  slug: string
  content: string
  excerpt?: string
  authorId: string
  published: boolean
  featured: boolean
  image?: string
  views: number
  createdAt: Date
  publishedAt?: Date
  author: User
  tags: PostTag[]
}

export type PostTag = {
  id: string
  name: string
}

export type Comment = {
  id: string
  content: string
  postId: string
  authorId: string
  parentId?: string
  createdAt: Date
  author: User
  replies: Comment[]
}

export type Notification = {
  id: string
  userId: string
  type: 'ORDER_UPDATE' | 'FORUM_REPLY' | 'COMMENT_REPLY' | 'SYSTEM'
  title: string
  message: string
  read: boolean
  link?: string
  createdAt: Date
}

